//! The Data tab: summary cards and a "storage by host" chart, from local
//! aggregate stats (fetched and cached per connection).

use eframe::egui;
use std::time::{Duration, Instant};

use crate::app::format::{ago, human_size};
use crate::app::{ACCENT, App};
use crate::backend::Command;

impl App {
    pub(crate) fn data_view(&mut self, ui: &mut egui::Ui, cidx: usize) {
        let conn_id = self.connections[cidx].id;

        // Auto-fetch once when the tab is first opened.
        if self.connections[cidx].stats.is_none() && !self.connections[cidx].stats_pending {
            self.connections[cidx].stats_pending = true;
            let _ = self.cmd_tx.send(Command::FetchStats { conn_id });
        }

        ui.horizontal(|ui| {
            ui.heading("Data");
            if ui.button("Refresh").clicked() {
                self.connections[cidx].stats_pending = true;
                let _ = self.cmd_tx.send(Command::FetchStats { conn_id });
            }
            if let Some(t) = self.connections[cidx].stats_refreshed {
                ui.weak(format!("computed {}", ago(t.elapsed())));
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
        });
        ui.separator();

        let on_network = self.connections[cidx]
            .account
            .as_ref()
            .map(|a| a.pinned_size);
        // Scoped so the `stats` borrow ends before we set the copied toast.
        let copied = std::cell::Cell::new(false);
        {
            let Some(stats) = self.connections[cidx].stats.as_ref() else {
                ui.weak("Computing…");
                return;
            };

            // Summary cards.
            let redundancy = if stats.total_min_shards > 0 {
                stats.sector_count as f64 / stats.total_min_shards as f64
            } else {
                0.0
            };
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                card(ui, "Objects", &stats.object_count.to_string());
                card(
                    ui,
                    "Logical size",
                    &human_size(stats.total_size.max(0) as u64),
                );
                if let Some(n) = on_network {
                    card(ui, "On network", &human_size(n));
                }
                card(ui, "Slabs", &stats.slab_count.to_string());
                card(ui, "Sectors", &stats.sector_count.to_string());
                card(ui, "Distinct hosts", &stats.distinct_hosts.to_string());
                card(ui, "Redundancy", &format!("{redundancy:.1}×"));
            });

            ui.add_space(10.0);
            ui.heading("Storage by host");
            if stats.by_host.is_empty() {
                ui.weak("No slab/sector data yet — sync or re-sync objects.");
                return;
            }
            // Concentration: share of stored bytes held by the busiest host.
            let total: f64 = stats
                .by_host
                .iter()
                .map(|(_, _, b)| *b as f64)
                .sum::<f64>()
                .max(1.0);
            let top_share = stats.by_host[0].2 as f64 / total * 100.0;
            ui.weak(format!(
                "{} hosts · busiest holds {:.1}% of stored data",
                stats.distinct_hosts, top_share
            ));

            // A ranked bar list: host key, proportional bar, and bytes stored. Every
            // host is labelled and readable (egui_plot thins categorical labels).
            ui.add_space(6.0);
            let max = stats.by_host.first().map_or(1, |(_, _, b)| *b).max(1) as f32;
            let size_w = 96.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (i, (host, sectors, bytes)) in stats.by_host.iter().enumerate() {
                        ui.horizontal(|ui| {
                            // Rank, so the top-N are countable at a glance.
                            ui.add_sized(
                                [30.0, 16.0],
                                egui::Label::new(
                                    egui::RichText::new(format!("{:>3}", i + 1))
                                        .monospace()
                                        .weak(),
                                ),
                            );
                            // Key label grows with the window; full key on hover.
                            let label_w = (ui.available_width() * 0.3).clamp(140.0, 480.0);
                            let resp = ui
                                .add_sized(
                                    [label_w, 16.0],
                                    egui::Label::new(egui::RichText::new(host).monospace())
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_text(format!("{host}\n(click to copy)"));
                            if resp.clicked() {
                                ui.ctx().copy_text(host.clone());
                                copied.set(true);
                            }

                            // Leave room for the size cell plus inter-widget
                            // spacing and the scrollbar, so it isn't clipped.
                            let bar_w = (ui.available_width() - size_w - 16.0).max(40.0);
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(bar_w, 13.0), egui::Sense::hover());
                            let painter = ui.painter();
                            painter.rect_filled(rect, 2.0, egui::Color32::from_gray(38));
                            let fill = egui::Rect::from_min_size(
                                rect.min,
                                egui::vec2(rect.width() * (*bytes as f32 / max), rect.height()),
                            );
                            painter.rect_filled(fill, 2.0, ACCENT);

                            ui.allocate_ui_with_layout(
                                egui::vec2(size_w, 16.0),
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.monospace(human_size(*bytes as u64))
                                        .on_hover_text(format!("{sectors} sectors"));
                                },
                            );
                        });
                    }
                });
        }
        if copied.get() {
            self.copied_at = Some(Instant::now());
        }
    }
}

/// A small labelled stat card. Its text never wraps, so the card keeps its
/// natural width and `horizontal_wrapped` moves whole cards to the next row.
fn card(ui: &mut egui::Ui, label: &str, value: &str) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(value).size(20.0).strong().color(ACCENT));
            ui.weak(label);
        });
    });
}
