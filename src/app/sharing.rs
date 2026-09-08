//! The Sharing tab: the account's sharing keys and the objects attached to
//! each one. A sharing key's seed is the whole credential a recipient needs,
//! so it is shown here to be copied and handed out.

use eframe::egui;
use std::time::{Duration, Instant};

use crate::app::data::card;
use crate::app::format::{ago, human_size, short_time};
use crate::app::objects::metadata_preview;
use crate::app::{App, confirm_modal};
use crate::backend::{Command, SharingKeyInfo};

/// The "new sharing key" form.
pub(crate) struct CreateKeyDialog {
    pub(crate) conn_id: i64,
    pub(crate) description: String,
    pub(crate) expires: bool,
    /// How many `unit`s from now the key expires.
    pub(crate) amount: String,
    pub(crate) unit: ExpiryUnit,
}

/// The unit the create form's expiry is entered in.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ExpiryUnit {
    Minutes,
    Hours,
    Days,
}

impl ExpiryUnit {
    fn label(self) -> &'static str {
        match self {
            ExpiryUnit::Minutes => "minutes",
            ExpiryUnit::Hours => "hours",
            ExpiryUnit::Days => "days",
        }
    }

    fn secs(self) -> i64 {
        match self {
            ExpiryUnit::Minutes => 60,
            ExpiryUnit::Hours => 3600,
            ExpiryUnit::Days => 86400,
        }
    }
}

/// The "import sharing key" form.
pub(crate) struct ImportKeyDialog {
    pub(crate) conn_id: i64,
    /// The pasted credential (hex seed).
    pub(crate) seed: String,
    /// A local name for it; recipients aren't told the owner's description.
    pub(crate) label: String,
}

/// The object picker for attaching an object to a sharing key.
pub(crate) struct AttachDialog {
    pub(crate) conn_id: i64,
    pub(crate) key_seed: String,
    /// The key's description (or public key), for the window title.
    pub(crate) key_label: String,
    pub(crate) query: String,
}

/// Whether `s` is a 64-character hex string: the shape of both an object id
/// and a sharing key seed.
fn is_hex_256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl App {
    pub(crate) fn sharing_view(&mut self, ui: &mut egui::Ui, cidx: usize) {
        let conn_id = self.connections[cidx].id;

        // Auto-fetch once when the tab is first opened.
        if self.connections[cidx].sharing_keys.is_none() && !self.connections[cidx].sharing_pending
        {
            self.connections[cidx].sharing_pending = true;
            let _ = self.cmd_tx.send(Command::FetchSharingKeys { conn_id });
            let _ = self.cmd_tx.send(Command::FetchImportedKeys { conn_id });
        }

        ui.horizontal(|ui| {
            let owned = self.connections[cidx]
                .sharing_keys
                .as_ref()
                .map_or(0, Vec::len);
            let imported = self.connections[cidx].imported_keys.len();
            ui.heading(format!("Sharing keys ({})", owned + imported));
            if ui.button("➕ New key").clicked() {
                self.create_key = Some(CreateKeyDialog {
                    conn_id,
                    description: String::new(),
                    expires: false,
                    amount: "30".to_string(),
                    unit: ExpiryUnit::Days,
                });
            }
            if ui
                .button("Import key…")
                .on_hover_text("Use a sharing key someone gave you")
                .clicked()
            {
                self.import_key = Some(ImportKeyDialog {
                    conn_id,
                    seed: String::new(),
                    label: String::new(),
                });
            }
            if ui.button("Refresh").clicked() {
                self.connections[cidx].sharing_pending = true;
                let _ = self.cmd_tx.send(Command::FetchSharingKeys { conn_id });
                let _ = self.cmd_tx.send(Command::FetchImportedKeys { conn_id });
            }
            if let Some(t) = self.connections[cidx].sharing_refreshed {
                ui.weak(format!("refreshed {}", ago(t.elapsed())));
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
        });
        ui.separator();

        // Cloned so the panels below can take `&mut self` while drawing; an
        // account has few keys, so the copy is cheap.
        let owned = self.connections[cidx].sharing_keys.clone();
        let imported = self.connections[cidx].imported_keys.clone();
        if owned.is_none() && imported.is_empty() {
            ui.weak("Loading sharing keys…");
            return;
        }
        // An imported key the account also owns would otherwise appear twice.
        let owned_pks: Vec<&String> = owned
            .iter()
            .flatten()
            .map(|k: &SharingKeyInfo| &k.public_key)
            .collect();
        let keys: Vec<SharingKeyInfo> = owned
            .iter()
            .flatten()
            .cloned()
            .chain(
                imported
                    .iter()
                    .filter(|k| !owned_pks.contains(&&k.public_key))
                    .cloned(),
            )
            .collect();
        if owned.is_none() {
            ui.weak("The account's own keys couldn't be listed; showing imported keys only.");
        }
        if keys.is_empty() {
            ui.weak("No sharing keys yet. Create one, or import a key you were given.");
            return;
        }

        let selected: Option<SharingKeyInfo> = self
            .selected_key
            .as_ref()
            .and_then(|pk| keys.iter().find(|k| &k.public_key == pk).cloned());

        // Load the selected key's attachments once, on select.
        if let Some(key) = &selected {
            let cache_key = (conn_id, key.public_key.clone());
            if !self.shared_objects.contains_key(&cache_key)
                && !self.shared_pending.contains(&cache_key)
            {
                self.fetch_shared(conn_id, key);
            }
        }

        egui::Panel::right("sharing-detail")
            .resizable(true)
            .default_size(560.0)
            .size_range(360.0..=1100.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match &selected {
                        Some(key) => self.sharing_key_panel(ui, conn_id, key),
                        None => {
                            ui.weak("Select a key to see its details and attached objects.");
                        }
                    });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Truncate long descriptions/keys instead of wrapping.
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    for key in &keys {
                        let is_selected =
                            self.selected_key.as_deref() == Some(key.public_key.as_str());
                        let label = if key.unavailable {
                            format!("{}  (imported, unavailable)", description_of(key))
                        } else if key.imported {
                            format!(
                                "{}  ({} objs, imported)",
                                description_of(key),
                                key.object_count
                            )
                        } else {
                            format!("{}  ({} objs)", description_of(key), key.object_count)
                        };
                        let resp = ui
                            .selectable_label(is_selected, label)
                            .on_hover_text(&key.public_key);
                        if resp.clicked() {
                            self.selected_key = if is_selected {
                                None
                            } else {
                                Some(key.public_key.clone())
                            };
                        }
                        resp.context_menu(|ui| {
                            // Revoking is the owner's operation; an imported key
                            // can only be dropped locally.
                            if key.imported {
                                if ui.button("🗑 Forget").clicked() {
                                    let _ = self.cmd_tx.send(Command::ForgetSharingKey {
                                        conn_id,
                                        public_key: key.public_key.clone(),
                                    });
                                    ui.close();
                                }
                            } else if ui.button("🗑 Revoke").clicked() {
                                self.pending_key_revoke = Some((
                                    conn_id,
                                    key.seed.clone(),
                                    description_of(key).to_string(),
                                ));
                                ui.close();
                            }
                        });
                    }
                });
        });
    }

    /// The selected key's details, stats, and attached objects.
    fn sharing_key_panel(&mut self, ui: &mut egui::Ui, conn_id: i64, key: &SharingKeyInfo) {
        ui.add_space(4.0);
        ui.heading(description_of(key));

        egui::Grid::new("sharing_key_info")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.label("Public key");
                self.copyable(ui, &key.public_key, "(click to copy)");
                ui.end_row();

                ui.label("Key");
                self.copyable(
                    ui,
                    &key.seed,
                    "(the credential recipients need — click to copy)",
                );
                ui.end_row();

                ui.label("Created");
                ui.label(short_time(&key.created_at));
                ui.end_row();

                ui.label("Updated");
                ui.label(short_time(&key.updated_at));
                ui.end_row();

                ui.label("Expires");
                match &key.expires_at {
                    Some(t) => ui.label(short_time(t)),
                    None => ui.weak("never"),
                };
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            card(ui, "Objects", &key.object_count.to_string());
            card(ui, "Logical size", &human_size(key.object_size));
            card(ui, "Pinned data", &human_size(key.pinned_data));
            card(ui, "On network", &human_size(key.pinned_size));
        });

        if key.unavailable {
            ui.weak(
                "The indexer wouldn't serve this key: it may be revoked, expired, or from \
                 another indexer. The stats above are placeholders.",
            );
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            // Attaching and revoking are signed by the owner's app key, so they
            // are only offered for the account's own keys.
            if !key.imported && ui.button("➕ Attach object…").clicked() {
                self.attach_dialog = Some(AttachDialog {
                    conn_id,
                    key_seed: key.seed.clone(),
                    key_label: description_of(key).to_string(),
                    query: String::new(),
                });
            }
            if ui.button("Refresh").clicked() {
                self.shared_objects
                    .remove(&(conn_id, key.public_key.clone()));
                self.fetch_shared(conn_id, key);
            }
            if key.imported {
                if ui
                    .button("🗑 Forget")
                    .on_hover_text("Drops the key locally; the key itself is untouched")
                    .clicked()
                {
                    let _ = self.cmd_tx.send(Command::ForgetSharingKey {
                        conn_id,
                        public_key: key.public_key.clone(),
                    });
                    self.selected_key = None;
                }
            } else if ui
                .button("🗑 Revoke")
                .on_hover_text("Deletes the key and every attachment on it")
                .clicked()
            {
                self.pending_key_revoke =
                    Some((conn_id, key.seed.clone(), description_of(key).to_string()));
            }
        });

        ui.add_space(4.0);
        ui.separator();

        let Some(objects) = self
            .shared_objects
            .get(&(conn_id, key.public_key.clone()))
            .cloned()
        else {
            ui.weak("Loading attached objects…");
            return;
        };
        ui.heading(format!("Attached objects ({})", objects.len()));
        if objects.is_empty() {
            ui.weak("Nothing attached — the key grants access to no data.");
            return;
        }
        ui.weak("Downloads here go through the sharing key, as a recipient would.");
        for object in &objects {
            ui.horizontal(|ui| {
                if ui
                    .small_button("Download")
                    .on_hover_text("Download as a recipient, using this key")
                    .clicked()
                {
                    // The save dialog is opened once the panels are drawn.
                    self.shared_download = Some((conn_id, key.seed.clone(), object.id.clone()));
                }
                // Detaching re-writes the owner's attachment list.
                if !key.imported
                    && ui
                        .small_button("Detach")
                        .on_hover_text("Revoke access to this object")
                        .clicked()
                {
                    let _ = self.cmd_tx.send(Command::UnshareObject {
                        conn_id,
                        key_seed: key.seed.clone(),
                        object_id: object.id.clone(),
                    });
                    // The backend re-emits the list once the indexer confirms.
                    self.shared_objects
                        .remove(&(conn_id, key.public_key.clone()));
                    self.shared_pending
                        .insert((conn_id, key.public_key.clone()));
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(80.0, 16.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.monospace(human_size(object.size))
                            .on_hover_text(format!("{} slabs", object.slabs));
                    },
                );
                let hint = if object.metadata.is_empty() {
                    "(click to copy)".to_string()
                } else {
                    format!("{}\n(click to copy)", metadata_preview(&object.metadata))
                };
                self.copyable(ui, &object.id, &hint);
            });
        }
    }

    /// Renders the Sharing tab's windows: the create form, the attach picker,
    /// and the revoke confirmation.
    pub(crate) fn sharing_windows(&mut self, ctx: &egui::Context) {
        self.create_key_dialog(ctx);
        self.import_key_dialog(ctx);
        self.attach_object_dialog(ctx);

        if let Some((conn_id, key_seed, object_id)) = self.shared_download.take()
            && let Some(dest) = rfd::FileDialog::new().set_file_name(&object_id).save_file()
        {
            // Progress shows up in the downloads window, marked as going
            // through the key.
            let _ = self.cmd_tx.send(Command::DownloadSharedObject {
                conn_id,
                key_seed,
                object_id,
                dest,
            });
        }

        if let Some((conn_id, key_seed, description)) = self.pending_key_revoke.clone() {
            match confirm_modal(
                ctx,
                "Revoke sharing key",
                &format!("Revoke the sharing key \"{description}\"?"),
                None,
                "Deletes the key and all of its attachments, revoking recipients' \
                 access. Downloads already in flight may keep working for a few \
                 minutes.",
                "Revoke",
            ) {
                Some(true) => {
                    let _ = self
                        .cmd_tx
                        .send(Command::RevokeSharingKey { conn_id, key_seed });
                    self.pending_key_revoke = None;
                }
                Some(false) => self.pending_key_revoke = None,
                None => {}
            }
        }
    }

    fn create_key_dialog(&mut self, ctx: &egui::Context) {
        let mut create: Option<(i64, String, Option<i64>)> = None;
        let mut close = false;
        if let Some(d) = &mut self.create_key {
            egui::Window::new("New sharing key")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    egui::Grid::new("new_sharing_key")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Description");
                            ui.add(
                                egui::TextEdit::singleline(&mut d.description)
                                    .hint_text("e.g. photos for alice")
                                    .desired_width(320.0),
                            );
                            ui.end_row();

                            ui.label("Expiry");
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut d.expires, "expires in");
                                ui.add_enabled_ui(d.expires, |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut d.amount)
                                            .desired_width(60.0),
                                    );
                                    egui::ComboBox::from_id_salt("expiry_unit")
                                        .selected_text(d.unit.label())
                                        .width(90.0)
                                        .show_ui(ui, |ui| {
                                            for u in [
                                                ExpiryUnit::Minutes,
                                                ExpiryUnit::Hours,
                                                ExpiryUnit::Days,
                                            ] {
                                                ui.selectable_value(&mut d.unit, u, u.label());
                                            }
                                        });
                                });
                            });
                            ui.end_row();
                        });

                    // The indexer wants an absolute expiry; send it as a delay in
                    // seconds so the backend can apply its own clock.
                    let secs = d
                        .amount
                        .trim()
                        .parse::<i64>()
                        .ok()
                        .filter(|n| *n > 0)
                        // checked, so a huge amount is rejected rather than
                        // overflowing into a bogus expiry
                        .and_then(|n| n.checked_mul(d.unit.secs()));
                    if d.expires && secs.is_none() {
                        ui.colored_label(
                            egui::Color32::RED,
                            format!("Enter a positive number of {}.", d.unit.label()),
                        );
                    }
                    ui.small("The key's holder can list and download whatever you attach to it.");

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let ready = !d.expires || secs.is_some();
                        if ui.add_enabled(ready, egui::Button::new("Create")).clicked() {
                            create = Some((
                                d.conn_id,
                                d.description.trim().to_string(),
                                secs.filter(|_| d.expires),
                            ));
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
        }
        if let Some((conn_id, description, expires_in_secs)) = create {
            let _ = self.cmd_tx.send(Command::CreateSharingKey {
                conn_id,
                description,
                expires_in_secs,
            });
            self.status = "Creating sharing key…".to_string();
            self.create_key = None;
        }
        if close {
            self.create_key = None;
        }
    }

    fn import_key_dialog(&mut self, ctx: &egui::Context) {
        let mut import: Option<(i64, String, String)> = None;
        let mut close = false;
        if let Some(d) = &mut self.import_key {
            egui::Window::new("Import sharing key")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    egui::Grid::new("import_sharing_key")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Key");
                            ui.add(
                                egui::TextEdit::singleline(&mut d.seed)
                                    .hint_text("64 hex characters, as copied from the owner")
                                    .desired_width(420.0),
                            );
                            ui.end_row();

                            ui.label("Label");
                            ui.add(
                                egui::TextEdit::singleline(&mut d.label)
                                    .hint_text("optional, e.g. alice's photos")
                                    .desired_width(420.0),
                            );
                            ui.end_row();
                        });

                    let seed = d.seed.trim().to_string();
                    if !seed.is_empty() && !is_hex_256(&seed) {
                        ui.colored_label(
                            egui::Color32::RED,
                            "A sharing key is 64 hex characters.",
                        );
                    }
                    ui.small(
                        "Checked against this connection's indexer, then kept locally.                          The key is read-only: you can list and download what it grants.",
                    );

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(is_hex_256(&seed), egui::Button::new("Import"))
                            .clicked()
                        {
                            import = Some((d.conn_id, seed, d.label.trim().to_string()));
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
        }
        if let Some((conn_id, seed, label)) = import {
            let _ = self.cmd_tx.send(Command::ImportSharingKey {
                conn_id,
                seed,
                label,
            });
            self.status = "Importing sharing key…".to_string();
            self.import_key = None;
        }
        if close {
            self.import_key = None;
        }
    }

    fn attach_object_dialog(&mut self, ctx: &egui::Context) {
        // The connection's synced objects, so the picker can list them.
        let objects: Vec<(String, u64)> = self
            .attach_dialog
            .as_ref()
            .and_then(|d| self.connections.iter().find(|c| c.id == d.conn_id))
            .map(|c| c.objects.iter().map(|o| (o.id.clone(), o.size)).collect())
            .unwrap_or_default();

        let mut attach: Option<(i64, String, String)> = None;
        let mut close = false;
        if let Some(d) = &mut self.attach_dialog {
            egui::Window::new(format!("Attach object to “{}”", d.key_label))
                .collapsible(false)
                .resizable(true)
                .default_size([620.0, 420.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Object id");
                        ui.add(
                            egui::TextEdit::singleline(&mut d.query)
                                .hint_text("filter synced objects, or paste a full id")
                                .desired_width(420.0),
                        );
                        // An id that isn't synced locally can still be attached.
                        if ui
                            .add_enabled(is_hex_256(d.query.trim()), egui::Button::new("Attach"))
                            .clicked()
                        {
                            attach =
                                Some((d.conn_id, d.key_seed.clone(), d.query.trim().to_string()));
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                    ui.separator();

                    let query = d.query.trim().to_lowercase();
                    let matches: Vec<&(String, u64)> = objects
                        .iter()
                        .filter(|(id, _)| query.is_empty() || id.contains(&query))
                        .collect();
                    ui.weak(format!(
                        "{} of {} synced objects",
                        matches.len(),
                        objects.len()
                    ));
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            for (id, size) in matches {
                                ui.horizontal(|ui| {
                                    if ui.small_button("Attach").clicked() {
                                        attach = Some((d.conn_id, d.key_seed.clone(), id.clone()));
                                    }
                                    ui.monospace(human_size(*size));
                                    ui.monospace(id).on_hover_text(id);
                                });
                            }
                        });
                });
        }
        if let Some((conn_id, key_seed, object_id)) = attach {
            let _ = self.cmd_tx.send(Command::ShareObject {
                conn_id,
                key_seed,
                object_id,
            });
            self.status = "Attaching object…".to_string();
            self.attach_dialog = None;
        }
        if close {
            self.attach_dialog = None;
        }
    }

    /// Requests the objects attached to `key`, marking the fetch pending.
    fn fetch_shared(&mut self, conn_id: i64, key: &SharingKeyInfo) {
        self.shared_pending
            .insert((conn_id, key.public_key.clone()));
        let _ = self.cmd_tx.send(Command::FetchSharedObjects {
            conn_id,
            key_seed: key.seed.clone(),
            imported: key.imported,
        });
    }

    /// A truncated monospace hash that copies itself when clicked; `hint` is
    /// appended to the hover text on its own line.
    fn copyable(&mut self, ui: &mut egui::Ui, text: &str, hint: &str) {
        let resp = ui
            .add(
                egui::Label::new(egui::RichText::new(text).monospace())
                    .truncate()
                    .sense(egui::Sense::click()),
            )
            .on_hover_text(format!("{text}\n{hint}"));
        if resp.clicked() {
            ui.ctx().copy_text(text.to_string());
            self.copied_at = Some(Instant::now());
        }
    }
}

/// A key's description, falling back to a placeholder for unnamed keys.
fn description_of(key: &SharingKeyInfo) -> &str {
    if key.description.trim().is_empty() {
        "(no description)"
    } else {
        &key.description
    }
}
