//! The Hosts tab: a filterable host list, a world map, and per-host details.

use eframe::egui;
use std::time::Duration;

use crate::app::format::{ago, usd};
use crate::app::{ACCENT, App};
use crate::backend::{Command, HostDetailInfo, HostInfo};

/// Cached siascan settings/prices for a host, keyed by public key.
#[derive(Clone)]
pub(crate) enum HostDetail {
    Loading,
    Ready(Box<HostDetailInfo>),
    Missing,
}

/// A host's location and identity, plotted on the hosts world map.
struct MapPoint {
    pubkey: String,
    country: String,
    addresses: Vec<String>,
    lat: f64,
    lng: f64,
    good: bool,
}

/// Filter for a host's upload suitability.
#[derive(Clone, Copy, PartialEq, Default)]
pub(crate) enum GoodFilter {
    #[default]
    Any,
    Good,
    NotGood,
}

impl GoodFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            GoodFilter::Any => "any",
            GoodFilter::Good => "yes",
            GoodFilter::NotGood => "no",
        }
    }
}

/// Whether a host matches the host-tab filters.
fn host_matches(h: &HostInfo, query: &str, good: GoodFilter) -> bool {
    let good_ok = match good {
        GoodFilter::Any => true,
        GoodFilter::Good => h.good_for_upload,
        GoodFilter::NotGood => !h.good_for_upload,
    };
    good_ok
        && (query.is_empty()
            || h.public_key.to_lowercase().contains(query)
            || h.addresses.iter().any(|a| a.to_lowercase().contains(query)))
}

impl App {
    /// Renders the Hosts tab: a filterable list on the left; selecting a row or
    /// map dot shows that host's details on the right, below the world map.
    pub(crate) fn hosts_view(&mut self, ui: &mut egui::Ui, cidx: usize) {
        let conn_id = self.connections[cidx].id;
        let has_hosts = self.connections[cidx].hosts.is_some();
        let total = self.connections[cidx].hosts.as_ref().map_or(0, Vec::len);

        // Filter using the current (last frame's) filter state.
        let query = self.host_filter.trim().to_lowercase();
        let good = self.host_good;
        let filter_active = !query.is_empty() || good != GoodFilter::Any;
        let filtered: Vec<usize> = self.connections[cidx]
            .hosts
            .as_ref()
            .map(|hosts| {
                hosts
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| host_matches(h, &query, good))
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();

        ui.horizontal(|ui| {
            ui.heading(if filter_active {
                format!("Hosts ({} of {total})", filtered.len())
            } else {
                format!("Hosts ({total})")
            });
            if ui.button("Refresh").clicked() {
                let _ = self.cmd_tx.send(Command::RefreshHosts { conn_id });
            }
            if let Some(t) = self.connections[cidx].hosts_refreshed {
                ui.weak(format!("refreshed {}", ago(t.elapsed())));
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.show_filters, "Filters");
            });
        });
        if self.show_filters {
            egui::Grid::new("host_filters_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Public key / address contains");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.host_filter)
                            .hint_text("substring")
                            .desired_width(320.0),
                    );
                    ui.end_row();

                    ui.label("Good for upload");
                    egui::ComboBox::from_id_salt("host_good")
                        .selected_text(self.host_good.label())
                        .show_ui(ui, |ui| {
                            for g in [GoodFilter::Any, GoodFilter::Good, GoodFilter::NotGood] {
                                ui.selectable_value(&mut self.host_good, g, g.label());
                            }
                        });
                    ui.end_row();
                });
            if ui.button("Clear filters").clicked() {
                self.host_filter.clear();
                self.host_good = GoodFilter::Any;
            }
        }
        ui.separator();

        if !has_hosts {
            ui.weak("Loading hosts…");
            return;
        }

        // The selected host, cloned so drawing doesn't hold a borrow while the
        // selection/details are mutated. Selection may point at a filtered-out
        // host; that just shows no details.
        let selected_info: Option<HostInfo> = self.selected_host.as_ref().and_then(|pk| {
            self.connections[cidx]
                .hosts
                .as_ref()?
                .iter()
                .find(|h| &h.public_key == pk)
                .cloned()
        });

        // Fetch siascan settings/prices for the selected host once, on select.
        if let Some(h) = &selected_info
            && !self.host_details.contains_key(&h.public_key)
        {
            self.host_details
                .insert(h.public_key.clone(), HostDetail::Loading);
            let _ = self.cmd_tx.send(Command::FetchHostDetail {
                public_key: h.public_key.clone(),
            });
        }
        let selected_detail: Option<HostDetail> = selected_info
            .as_ref()
            .and_then(|h| self.host_details.get(&h.public_key).cloned());

        // Owned map points, so drawing the map doesn't borrow the connection
        // while the selection is mutated.
        let points: Vec<MapPoint> = {
            let hosts = self.connections[cidx].hosts.as_ref().unwrap();
            filtered
                .iter()
                .map(|&i| {
                    let h = &hosts[i];
                    MapPoint {
                        pubkey: h.public_key.clone(),
                        country: h.country.clone(),
                        addresses: h.addresses.clone(),
                        lat: h.latitude,
                        lng: h.longitude,
                        good: h.good_for_upload,
                    }
                })
                .collect()
        };

        // Right panel: map on top, selected host's details below. The list lives
        // in a CentralPanel so it can't overlap the SidePanel before it's resized.
        let refresh_selected = std::cell::Cell::new(false);
        egui::Panel::right("host-map-panel")
            .resizable(true)
            .default_size(520.0)
            .size_range(300.0..=1000.0)
            .show(ui, |ui| {
                self.host_map(ui, &points);
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match &selected_info {
                        Some(host) => {
                            if host_details_panel(ui, host, selected_detail.as_ref()) {
                                refresh_selected.set(true);
                            }
                        }
                        None => {
                            ui.weak("Select a host (row or map dot) to see its details.");
                        }
                    });
            });

        let hosts = self.connections[cidx].hosts.as_ref().unwrap();
        let selected = self.selected_host.clone();
        // Consume the "scroll to selection" request set by a map-dot click.
        let scroll_pending = self.scroll_to_selected;
        self.scroll_to_selected = false;
        // Host clicked-to-select this frame (applied after the borrow ends).
        let clicked: std::cell::Cell<Option<String>> = std::cell::Cell::new(None);
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Shorten long public keys with an ellipsis instead of wrapping.
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    for &i in &filtered {
                        let host = &hosts[i];
                        let is_selected = selected.as_deref() == Some(host.public_key.as_str());
                        let text = egui::RichText::new(&host.public_key).monospace();
                        let resp = ui
                            .selectable_label(is_selected, text)
                            .on_hover_text(&host.public_key);
                        if resp.clicked() {
                            clicked.set(Some(host.public_key.clone()));
                        }
                        if is_selected && scroll_pending {
                            resp.scroll_to_me(Some(egui::Align::Center));
                        }
                    }
                });
        });

        if let Some(pubkey) = clicked.take() {
            // Toggle selection off if the already-selected row is clicked again.
            self.selected_host = if self.selected_host.as_deref() == Some(pubkey.as_str()) {
                None
            } else {
                Some(pubkey)
            };
        }
        // Re-fetch the selected host's siascan details on demand.
        if refresh_selected.get()
            && let Some(pk) = self.selected_host.clone()
        {
            self.host_details.insert(pk.clone(), HostDetail::Loading);
            let _ = self
                .cmd_tx
                .send(Command::FetchHostDetail { public_key: pk });
        }
    }

    /// Draws an equirectangular world map with a dot per host (green =
    /// upload-eligible). Clicking a dot selects that host; scroll zooms toward
    /// the cursor, drag pans, double-click resets.
    fn host_map(&mut self, ui: &mut egui::Ui, points: &[MapPoint]) {
        // Take only the height a 2:1 map needs, leaving room for the details
        // table below.
        let width = ui.available_width();
        let map_h = (width * 0.5).min((ui.available_height() - 8.0).max(120.0));
        let (resp, painter) =
            ui.allocate_painter(egui::vec2(width, map_h), egui::Sense::click_and_drag());
        let avail = resp.rect;
        let rounding = egui::CornerRadius::same(6);

        // Keep the 2:1 aspect ratio (else continents stretch); anchor to the top,
        // centered horizontally, so it aligns with the list.
        let (mw, mh) = if avail.width() / avail.height() > 2.0 {
            (avail.height() * 2.0, avail.height())
        } else {
            (avail.width(), avail.width() / 2.0)
        };
        let left = avail.left() + (avail.width() - mw) / 2.0;
        let rect = egui::Rect::from_min_size(egui::pos2(left, avail.top()), egui::vec2(mw, mh));
        let (w, h) = (rect.width(), rect.height());

        // --- Pan / zoom -------------------------------------------------------
        let (mut z, mut vx, mut vy) = (self.map_zoom, self.map_vx, self.map_vy);
        let on_map = resp.hover_pos().is_some_and(|p| rect.contains(p));
        if resp.double_clicked() {
            (z, vx, vy) = (1.0, 0.0, 0.0);
        } else {
            if resp.dragged() {
                let d = resp.drag_delta();
                vx -= d.x / (z * w);
                vy -= d.y / (z * h);
            }
            if on_map {
                let (scroll_y, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
                let factor = pinch * (scroll_y * 0.0015).exp();
                if let Some(c) = resp.hover_pos().filter(|_| (factor - 1.0).abs() > 1e-4) {
                    // Keep the world point under the cursor fixed while zooming.
                    let nx = vx + (c.x - rect.min.x) / (z * w);
                    let ny = vy + (c.y - rect.min.y) / (z * h);
                    z = (z * factor).clamp(1.0, 12.0);
                    vx = nx - (c.x - rect.min.x) / (z * w);
                    vy = ny - (c.y - rect.min.y) / (z * h);
                }
            }
        }
        // Clamp so the view can't leave the world.
        let maxv = (1.0 - 1.0 / z).max(0.0);
        vx = vx.clamp(0.0, maxv);
        vy = vy.clamp(0.0, maxv);
        (self.map_zoom, self.map_vx, self.map_vy) = (z, vx, vy);
        if resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }

        // Equirectangular projection with the current pan/zoom applied.
        let project = |lat: f64, lng: f64| -> egui::Pos2 {
            let nx = ((lng + 180.0) / 360.0) as f32;
            let ny = ((90.0 - lat) / 180.0) as f32;
            egui::pos2(
                rect.min.x + (nx - vx) * z * w,
                rect.min.y + (ny - vy) * z * h,
            )
        };

        painter.rect_filled(rect, rounding, egui::Color32::from_rgb(15, 22, 28));
        // Clip map content so zoomed-in geometry stays within the map rect.
        let mp = painter.with_clip_rect(rect);

        // Faint equator / prime meridian for orientation.
        let grid = egui::Stroke::new(1.0, egui::Color32::from_gray(34));
        mp.line_segment([project(0.0, -180.0), project(0.0, 180.0)], grid);
        mp.line_segment([project(90.0, 0.0), project(-90.0, 0.0)], grid);

        // Coastlines from the bundled world basemap.
        let coast = egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 88, 96));
        for ring in crate::worldmap::coastlines() {
            let pts: Vec<egui::Pos2> = ring
                .iter()
                .map(|&[lng, lat]| project(lat as f64, lng as f64))
                .collect();
            mp.add(egui::Shape::line(pts, coast));
        }

        // (0, 0) is the SDK's "unknown location" sentinel; don't plot a fake dot.
        let has_loc = |p: &MapPoint| p.lat != 0.0 || p.lng != 0.0;

        // Nearest dot to the cursor for hover/tooltip/selection (suppressed while
        // panning so the tooltip doesn't flicker).
        let mut hovered: Option<usize> = None;
        if !resp.dragged()
            && let Some(hp) = resp.hover_pos().filter(|p| rect.contains(*p))
        {
            let mut best = 9.0_f32;
            for (idx, p) in points.iter().enumerate() {
                if has_loc(p) {
                    let d = hp.distance(project(p.lat, p.lng));
                    if d < best {
                        best = d;
                        hovered = Some(idx);
                    }
                }
            }
        }

        let selected = self.selected_host.as_deref();
        for (idx, p) in points.iter().enumerate() {
            if !has_loc(p) {
                continue;
            }
            let pos = project(p.lat, p.lng);
            if !rect.contains(pos) {
                continue;
            }
            let is_sel = selected == Some(p.pubkey.as_str());
            let (r, color) = if is_sel {
                (4.5, ACCENT)
            } else if hovered == Some(idx) {
                (4.0, egui::Color32::from_rgb(130, 225, 170))
            } else if p.good {
                (
                    2.5,
                    egui::Color32::from_rgba_unmultiplied(45, 200, 110, 190),
                )
            } else {
                (
                    2.5,
                    egui::Color32::from_rgba_unmultiplied(150, 150, 150, 130),
                )
            };
            mp.circle_filled(pos, r, color);
            if is_sel {
                mp.circle_stroke(pos, r + 2.5, egui::Stroke::new(1.5, egui::Color32::WHITE));
            }
        }

        painter.rect_stroke(
            rect,
            rounding,
            egui::Stroke::new(1.0, egui::Color32::from_gray(72)),
            egui::StrokeKind::Inside,
        );

        if let Some(idx) = hovered {
            let p = &points[idx];
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                egui::Id::new("host-map-tip"),
                egui::PopupAnchor::Pointer,
            )
            .show(|ui| {
                // Let the tooltip grow to fit rather than wrapping the hashes.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(egui::RichText::new(&p.pubkey).monospace());
                ui.label(if p.country.is_empty() {
                    "location unknown".to_string()
                } else {
                    p.country.clone()
                });
                for addr in &p.addresses {
                    ui.monospace(addr);
                }
            });
            // Select on a plain click (a drag pans instead).
            if resp.clicked() {
                self.selected_host = Some(p.pubkey.clone());
                self.scroll_to_selected = true;
            }
        }

        // Caption: dot count, legend, and interaction hint.
        let plotted = points.iter().filter(|p| has_loc(p)).count();
        painter.text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{plotted} hosts · green = good · scroll to zoom, drag to pan"),
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(140),
        );
    }
}

/// Renders the selected host's details below the map. Returns `true` if a
/// refresh of the siascan data was requested.
fn host_details_panel(ui: &mut egui::Ui, host: &HostInfo, detail: Option<&HostDetail>) -> bool {
    let mut refresh = false;
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(&host.public_key).monospace().strong()).truncate(),
        )
        .on_hover_text(&host.public_key);
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(host.public_key.clone());
        }
    });
    ui.add_space(4.0);

    let id = egui::Id::new(("host-details", &host.public_key));
    egui::Grid::new(id.with("info"))
        .num_columns(2)
        .spacing([16.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label("Addresses");
            ui.vertical(|ui| {
                if host.addresses.is_empty() {
                    ui.weak("—");
                }
                for a in &host.addresses {
                    ui.monospace(a);
                }
            });
            ui.end_row();

            ui.label("Country");
            ui.label(if host.country.is_empty() {
                "—"
            } else {
                &host.country
            });
            ui.end_row();

            ui.label("Location");
            ui.label(format!("{:.4}, {:.4}", host.latitude, host.longitude));
            ui.end_row();

            ui.label("Good for upload");
            if host.good_for_upload {
                ui.colored_label(egui::Color32::from_rgb(60, 160, 60), "yes");
            } else {
                ui.label("no");
            }
            ui.end_row();
        });

    ui.separator();
    match detail {
        Some(HostDetail::Ready(d)) => {
            host_detail_grid(ui, id, d);
            if ui.button("Refresh").clicked() {
                refresh = true;
            }
        }
        Some(HostDetail::Loading) | None => {
            ui.weak("Loading settings & pricing…");
        }
        Some(HostDetail::Missing) => {
            ui.weak("Settings & pricing unavailable (host not on siascan).");
            if ui.button("Retry").clicked() {
                refresh = true;
            }
        }
    }
    refresh
}

fn host_detail_grid(ui: &mut egui::Ui, id: egui::Id, d: &HostDetailInfo) {
    let green = egui::Color32::from_rgb(60, 160, 60);
    egui::Grid::new(id.with("pricing"))
        .num_columns(2)
        .spacing([16.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label("Accepting contracts");
            if d.accepting_contracts {
                ui.colored_label(green, "yes");
            } else {
                ui.label("no");
            }
            ui.end_row();

            ui.label("Version");
            ui.label(if d.protocol_version.is_empty() {
                "—"
            } else {
                &d.protocol_version
            });
            ui.end_row();

            ui.label("Release");
            ui.label(if d.release.is_empty() {
                "—"
            } else {
                &d.release
            });
            ui.end_row();

            ui.label("Storage");
            ui.label(format!(
                "{:.2} TB total · {:.2} TB free",
                d.total_storage_tb, d.remaining_storage_tb
            ));
            ui.end_row();

            ui.label("Max duration");
            ui.label(format!("{:.0} days", d.max_contract_duration_days));
            ui.end_row();

            ui.label("Storage price");
            ui.label(format!("{} / TB / month", usd(d.storage_usd_tb_month)));
            ui.end_row();

            ui.label("Upload price");
            ui.label(format!("{} / TB", usd(d.ingress_usd_tb)));
            ui.end_row();

            ui.label("Download price");
            ui.label(format!("{} / TB", usd(d.egress_usd_tb)));
            ui.end_row();

            ui.label("Contract price");
            ui.label(usd(d.contract_usd));
            ui.end_row();

            ui.label("Collateral");
            ui.label(format!("{} / TB / month", usd(d.collateral_usd_tb_month)));
            ui.end_row();
        });
    ui.weak("settings & prices via siascan");
}
