//! The Logs panel: a filterable tail of revelio's own log records, toggled
//! from the top bar.

use eframe::egui;
use log::{Level, LevelFilter};
use std::time::{Duration, Instant};

use crate::app::App;

/// Level filters offered in the panel, coarsest first.
const LEVELS: [LevelFilter; 5] = [
    LevelFilter::Error,
    LevelFilter::Warn,
    LevelFilter::Info,
    LevelFilter::Debug,
    LevelFilter::Trace,
];

fn level_color(level: Level) -> egui::Color32 {
    match level {
        Level::Error => egui::Color32::from_rgb(220, 80, 80),
        Level::Warn => egui::Color32::from_rgb(220, 170, 60),
        Level::Info => egui::Color32::from_rgb(120, 180, 230),
        Level::Debug | Level::Trace => egui::Color32::from_gray(140),
    }
}

impl App {
    /// Draws the bottom log panel. Adding it before the other panels keeps it
    /// spanning the full window width.
    pub(crate) fn logs_panel(&mut self, ui: &mut egui::Ui) {
        if !self.show_logs {
            return;
        }
        // Records arrive from backend threads, which don't repaint the UI.
        ui.ctx().request_repaint_after(Duration::from_millis(300));

        let (lines, total) = self.logs.snapshot(self.log_level, &self.log_filter);
        let mut copy = false;
        egui::Panel::bottom("logs")
            .resizable(true)
            .default_size(240.0)
            .size_range(80.0..=700.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading("Logs");
                    egui::ComboBox::from_id_salt("log_level")
                        .selected_text(self.log_level.as_str())
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            for l in LEVELS {
                                ui.selectable_value(&mut self.log_level, l, l.as_str());
                            }
                        });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.log_filter)
                            .hint_text("filter target or message")
                            .desired_width(260.0),
                    );
                    ui.checkbox(&mut self.log_follow, "Follow")
                        .on_hover_text("Stay scrolled to the newest record");
                    if ui.button("Copy").clicked() {
                        copy = true;
                    }
                    if ui.button("Clear").clicked() {
                        self.logs.clear();
                    }
                    ui.weak(format!("{} of {total} shown", lines.len()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").on_hover_text("Hide logs").clicked() {
                            self.show_logs = false;
                        }
                    });
                });
                ui.separator();

                if lines.is_empty() {
                    ui.weak(if total == 0 {
                        "Nothing logged yet."
                    } else {
                        "No records match the filter."
                    });
                    return;
                }

                // One row per record (never wrapped) so the list can be
                // virtualized; the full line is on hover.
                let font = egui::TextStyle::Monospace.resolve(ui.style());
                let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
                let time_w = text_width(ui, &font, "00:00:00.000");
                let level_w = text_width(ui, &font, "WARN");
                let target_w = text_width(ui, &font, "revelio::backend");
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.log_follow)
                    .show_rows(ui, row_height, lines.len(), |ui, range| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                        for line in &lines[range] {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                let cell = |ui: &mut egui::Ui, w: f32, text: egui::RichText| {
                                    ui.add_sized([w, row_height], egui::Label::new(text));
                                };
                                cell(ui, time_w, mono(&line.time).weak());
                                cell(
                                    ui,
                                    level_w,
                                    mono(line.level.as_str()).color(level_color(line.level)),
                                );
                                cell(ui, target_w, mono(&line.target).weak());
                                ui.add(egui::Label::new(mono(&line.message)).truncate())
                                    .on_hover_text(&line.message);
                            });
                        }
                    });
            });

        if copy {
            let text: String = lines
                .iter()
                .map(|l| format!("{} {} {} {}\n", l.time, l.level, l.target, l.message))
                .collect();
            ui.ctx().copy_text(text);
            self.copied_at = Some(Instant::now());
        }
    }
}

fn mono(text: &str) -> egui::RichText {
    egui::RichText::new(text).monospace()
}

/// The rendered width of `sample`, used to size the fixed columns.
fn text_width(ui: &egui::Ui, font: &egui::FontId, sample: &str) -> f32 {
    ui.ctx().fonts_mut(|f| {
        f.layout_no_wrap(sample.to_string(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    })
}
