//! The downloads and uploads progress windows, and their shared transfer card.

use eframe::egui;

use crate::app::format::{fmt_duration, human_size};
use crate::app::{ACCENT, App};

impl App {
    /// Renders the downloads window when any download is active or
    /// finished-but-not-dismissed.
    pub(crate) fn downloads_window(&mut self, ctx: &egui::Context) {
        if self.downloads.is_empty() {
            return;
        }
        let mut dismiss: Vec<String> = Vec::new();
        let downloads = &self.downloads;
        egui::Window::new("Downloads")
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                for d in downloads.values() {
                    // The saved filename is friendlier than the full object hash.
                    let title = std::path::Path::new(&d.dest)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(d.object_id.as_str());
                    let state = if let Some(err) = &d.error {
                        TransferState::Error(err)
                    } else if d.done {
                        TransferState::Done {
                            stats: done_stats(d.written, d.elapsed_secs),
                            detail: Some(&d.dest),
                        }
                    } else {
                        TransferState::Progress {
                            frac: frac(d.written, d.total),
                            text: format!("{} / {}", human_size(d.written), human_size(d.total)),
                        }
                    };
                    if transfer_card(ui, title, &d.object_id, state) {
                        dismiss.push(d.object_id.clone());
                    }
                }
            });
        for id in dismiss {
            self.downloads.remove(&id);
        }
    }

    /// Renders the uploads window while any upload is active or
    /// finished-but-not-dismissed.
    pub(crate) fn uploads_window(&mut self, ctx: &egui::Context) {
        if self.uploads.is_empty() {
            return;
        }
        let mut dismiss: Vec<u64> = Vec::new();
        let uploads = &self.uploads;
        egui::Window::new("Uploads")
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                for u in uploads.values() {
                    let state = if let Some(err) = &u.error {
                        TransferState::Error(err)
                    } else if u.done {
                        // Speed is reported against the source size (what the user
                        // uploaded), not the parity-inflated encoded bytes.
                        TransferState::Done {
                            stats: done_stats(u.size, u.elapsed_secs),
                            detail: u.object_id.as_deref(),
                        }
                    } else {
                        let f = frac(u.written, u.total);
                        TransferState::Progress {
                            frac: f,
                            text: format!("uploading {} · {:.0}%", human_size(u.size), f * 100.0),
                        }
                    };
                    if transfer_card(ui, &u.name, &u.name, state) {
                        dismiss.push(u.upload_id);
                    }
                }
            });
        for id in dismiss {
            self.uploads.remove(&id);
        }
    }
}

/// The visual state of a transfer (download or upload) card.
enum TransferState<'a> {
    Error(&'a str),
    /// A completion summary line, with an optional dimmed detail (path / id).
    Done {
        stats: String,
        detail: Option<&'a str>,
    },
    Progress {
        frac: f32,
        text: String,
    },
}

/// Renders one transfer as a bordered card, returning whether Dismiss was
/// clicked. Shared by the downloads and uploads windows.
fn transfer_card(ui: &mut egui::Ui, title: &str, title_hover: &str, state: TransferState) -> bool {
    let mut dismiss = false;
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(egui::RichText::new(title).strong()).truncate())
            .on_hover_text(title_hover);
        match state {
            TransferState::Error(err) => {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 80, 80),
                    format!("Failed: {err}"),
                );
                dismiss = ui.small_button("Dismiss").clicked();
            }
            TransferState::Done { stats, detail } => {
                ui.label(egui::RichText::new(stats).color(ACCENT));
                if let Some(detail) = detail {
                    ui.add(egui::Label::new(egui::RichText::new(detail).small().weak()).truncate())
                        .on_hover_text(detail);
                }
                dismiss = ui.small_button("Dismiss").clicked();
            }
            TransferState::Progress { frac, text } => {
                ui.add(egui::ProgressBar::new(frac).text(text));
            }
        }
    });
    ui.add_space(6.0);
    dismiss
}

/// Fraction written/total, clamped to `0.0..=1.0` (0 when total is unknown).
fn frac(written: u64, total: u64) -> f32 {
    if total > 0 {
        (written as f32 / total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A completion summary: size, elapsed time, and average speed.
fn done_stats(bytes: u64, elapsed_secs: Option<f64>) -> String {
    match elapsed_secs {
        Some(secs) => {
            let speed = if secs > 0.0 {
                (bytes as f64 / secs) as u64
            } else {
                0
            };
            format!(
                "Done · {} in {} ({}/s)",
                human_size(bytes),
                fmt_duration(secs),
                human_size(speed),
            )
        }
        None => format!("Done · {}", human_size(bytes)),
    }
}
