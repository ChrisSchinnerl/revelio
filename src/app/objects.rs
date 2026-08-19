//! The Objects tab: filters, the sortable/paginated table, and the upload and
//! metadata dialogs.

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::app::format::{human_size, short_time};
use crate::app::{ACCENT, App};
use crate::backend::{Command, ObjectView, SlabView};

/// A numeric comparison operator for filtering.
#[derive(PartialEq, Clone, Copy, Default)]
enum NumOp {
    #[default]
    Any,
    Gt,
    Lt,
    Eq,
}

impl NumOp {
    fn label(self) -> &'static str {
        match self {
            NumOp::Any => "any",
            NumOp::Gt => ">",
            NumOp::Lt => "<",
            NumOp::Eq => "=",
        }
    }

    fn test(self, actual: u64, threshold: u64) -> bool {
        match self {
            NumOp::Any => true,
            NumOp::Gt => actual > threshold,
            NumOp::Lt => actual < threshold,
            NumOp::Eq => actual == threshold,
        }
    }
}

/// A time comparison for the `updated_at` filter.
#[derive(PartialEq, Clone, Copy, Default)]
enum TimeOp {
    #[default]
    Any,
    Newer,
    Older,
}

impl TimeOp {
    fn label(self) -> &'static str {
        match self {
            TimeOp::Any => "any",
            TimeOp::Newer => "newer than",
            TimeOp::Older => "older than",
        }
    }

    /// Compares an RFC3339-UTC timestamp against the user's bound; both being
    /// RFC3339-UTC, lexicographic order matches chronological order.
    fn test(self, actual: &str, bound: &str) -> bool {
        if bound.is_empty() {
            return true;
        }
        match self {
            TimeOp::Any => true,
            TimeOp::Newer => actual > bound,
            TimeOp::Older => actual < bound,
        }
    }
}

/// The object-table filters.
#[derive(Default, Clone, PartialEq)]
pub(crate) struct Filters {
    /// Combined id search: object id, or (via the DB) slab id / sector root.
    pub(crate) id: String,
    size_op: NumOp,
    size_val: u64,
    slabs_op: NumOp,
    slabs_val: u64,
    meta_op: NumOp,
    meta_val: u64,
    meta_substr: String,
    /// When true, the metadata substring is a *negative* filter (excludes matches).
    meta_substr_invert: bool,
    updated_op: TimeOp,
    updated_val: String,
}

impl Filters {
    pub(crate) fn is_active(&self) -> bool {
        !self.id.trim().is_empty()
            || self.size_op != NumOp::Any
            || self.slabs_op != NumOp::Any
            || self.meta_op != NumOp::Any
            || !self.meta_substr.trim().is_empty()
            || (self.updated_op != TimeOp::Any && !self.updated_val.trim().is_empty())
    }

    /// `id_lower`/`meta_lower` are the trimmed, lowercased queries. `component_hit`
    /// is true when the id query matched a slab or sector id (resolved via the DB).
    /// Cheaper checks come first so the metadata scan is skipped when another
    /// filter already excludes the object.
    fn matches(
        &self,
        o: &ObjectView,
        id_lower: &str,
        meta_lower: &str,
        component_hit: bool,
    ) -> bool {
        (id_lower.is_empty() || o.id == id_lower || component_hit)
            && self.size_op.test(o.size, self.size_val)
            && self.slabs_op.test(o.slabs, self.slabs_val)
            && self.meta_op.test(o.metadata.len() as u64, self.meta_val)
            && self.updated_op.test(&o.updated_at, self.updated_val.trim())
            && (meta_lower.is_empty() || {
                let contains = String::from_utf8_lossy(&o.metadata)
                    .to_lowercase()
                    .contains(meta_lower);
                // `excludes` mode passes objects that do NOT contain the substring.
                contains != self.meta_substr_invert
            })
    }

    pub(crate) fn form(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("filters_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Id equals")
                    .on_hover_text("Exact match on an object id, slab id, or sector root");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.id)
                            .hint_text("full object, slab, or sector id")
                            .desired_width(320.0),
                    );
                    // ids are 64-char hex hashes; warn on any other length.
                    let n = self.id.trim().chars().count();
                    if n != 0 && n != 64 {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 170, 70),
                            format!("{n}/64 chars"),
                        );
                    }
                });
                ui.end_row();

                ui.label("Size (bytes)");
                ui.horizontal(|ui| {
                    num_op_combo(ui, "size_op", &mut self.size_op);
                    ui.add_enabled(
                        self.size_op != NumOp::Any,
                        egui::DragValue::new(&mut self.size_val).speed(1024.0),
                    );
                });
                ui.end_row();

                ui.label("Slabs");
                ui.horizontal(|ui| {
                    num_op_combo(ui, "slabs_op", &mut self.slabs_op);
                    ui.add_enabled(
                        self.slabs_op != NumOp::Any,
                        egui::DragValue::new(&mut self.slabs_val).speed(1.0),
                    );
                });
                ui.end_row();

                ui.label("Metadata size");
                ui.horizontal(|ui| {
                    num_op_combo(ui, "meta_op", &mut self.meta_op);
                    ui.add_enabled(
                        self.meta_op != NumOp::Any,
                        egui::DragValue::new(&mut self.meta_val).speed(1.0),
                    );
                });
                ui.end_row();

                ui.label("Metadata");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("meta_mode")
                        .selected_text(if self.meta_substr_invert {
                            "excludes"
                        } else {
                            "contains"
                        })
                        .width(96.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.meta_substr_invert, false, "contains");
                            ui.selectable_value(&mut self.meta_substr_invert, true, "excludes");
                        });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.meta_substr)
                            .hint_text("text substring")
                            .desired_width(212.0),
                    );
                });
                ui.end_row();

                ui.label("Updated");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("updated_op")
                        .selected_text(self.updated_op.label())
                        .width(96.0)
                        .show_ui(ui, |ui| {
                            for op in [TimeOp::Any, TimeOp::Newer, TimeOp::Older] {
                                ui.selectable_value(&mut self.updated_op, op, op.label());
                            }
                        });
                    ui.add_enabled(
                        self.updated_op != TimeOp::Any,
                        egui::TextEdit::singleline(&mut self.updated_val)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(180.0),
                    );
                });
                ui.end_row();
            });

        if ui.button("Clear filters").clicked() {
            *self = Filters::default();
        }
    }
}

/// A `>`/`<`/`==`/`any` selector for a [`NumOp`].
fn num_op_combo(ui: &mut egui::Ui, salt: &str, op: &mut NumOp) {
    egui::ComboBox::from_id_salt(salt)
        .selected_text(op.label())
        .width(64.0)
        .show_ui(ui, |ui| {
            for candidate in [NumOp::Any, NumOp::Gt, NumOp::Lt, NumOp::Eq] {
                ui.selectable_value(op, candidate, candidate.label());
            }
        });
}

/// A sortable object-table column.
#[derive(Clone, Copy, PartialEq, Default)]
pub(crate) enum SortCol {
    #[default]
    Updated,
    Size,
}

/// Sort direction.
#[derive(Clone, Copy, PartialEq, Default)]
pub(crate) enum SortDir {
    Asc,
    #[default]
    Desc,
}

impl SortDir {
    fn arrow(self) -> &'static str {
        match self {
            SortDir::Asc => " ▲",
            SortDir::Desc => " ▼",
        }
    }
}

/// Memoized filtered + sorted view, reused across frames until the filters,
/// sort, selected connection, or its objects change.
pub(crate) struct ViewCache {
    conn_id: i64,
    epoch: u64,
    filters: Filters,
    sort_col: SortCol,
    sort_dir: SortDir,
    pub(crate) indices: Vec<usize>,
}

impl App {
    /// Recomputes the filtered + sorted object indices for `cidx`, unless the
    /// cache is still valid (same connection, objects, filters, sort). Avoids
    /// re-scanning/sorting ~70k objects every frame.
    pub(crate) fn refresh_view_cache(&mut self, cidx: usize) {
        let conn = &self.connections[cidx];
        let valid = self.view_cache.as_ref().is_some_and(|c| {
            c.conn_id == conn.id
                && c.epoch == self.objects_epoch
                && c.filters == self.filters
                && c.sort_col == self.sort_col
                && c.sort_dir == self.sort_dir
        });
        if valid {
            return;
        }

        // Filter (objects arrive already in updated_at-descending order).
        let mut indices: Vec<usize> = if self.filters.is_active() {
            let id_lower = self.filters.id.trim().to_lowercase();
            let meta_lower = self.filters.meta_substr.trim().to_lowercase();
            conn.objects
                .iter()
                .enumerate()
                .filter(|(_, o)| {
                    let component_hit = self.component_matches.contains(&o.id);
                    self.filters
                        .matches(o, &id_lower, &meta_lower, component_hit)
                })
                .map(|(i, _)| i)
                .collect()
        } else {
            (0..conn.objects.len()).collect()
        };

        // Sort. Updated-descending is the natural order, so skip sorting for it.
        if !(self.sort_col == SortCol::Updated && self.sort_dir == SortDir::Desc) {
            let objects = &conn.objects;
            indices.sort_by(|&a, &b| {
                let ord = match self.sort_col {
                    SortCol::Size => objects[a].size.cmp(&objects[b].size),
                    SortCol::Updated => objects[a].updated_at.cmp(&objects[b].updated_at),
                };
                match self.sort_dir {
                    SortDir::Asc => ord,
                    SortDir::Desc => ord.reverse(),
                }
            });
        }

        self.view_cache = Some(ViewCache {
            conn_id: conn.id,
            epoch: self.objects_epoch,
            filters: self.filters.clone(),
            sort_col: self.sort_col,
            sort_dir: self.sort_dir,
            indices,
        });
    }

    pub(crate) fn upload_dialog(&mut self, ctx: &egui::Context) {
        let mut start: Option<(i64, PathBuf, Vec<u8>)> = None;
        let mut close = false;
        if let Some(d) = &mut self.upload_dialog {
            egui::Window::new("Upload object")
                .collapsible(false)
                .resizable(true)
                .default_size([520.0, 340.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Choose file…").clicked()
                            && let Some(p) = rfd::FileDialog::new().pick_file()
                        {
                            d.path = Some(p);
                        }
                        match &d.path {
                            Some(p) => {
                                ui.monospace(p.file_name().and_then(|n| n.to_str()).unwrap_or(""))
                            }
                            None => ui.weak("no file selected"),
                        };
                    });
                    ui.separator();

                    ui.label("Metadata (optional)");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut d.hex, false, "Text/JSON");
                        ui.selectable_value(&mut d.hex, true, "Hex");
                    });
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut d.metadata_text)
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        });

                    // Text/JSON is always encodable; hex must parse.
                    let metadata = if d.hex {
                        parse_hex_edit(&d.metadata_text)
                    } else {
                        Some(encode_metadata(&d.metadata_text))
                    };
                    if d.hex && metadata.is_none() {
                        ui.colored_label(egui::Color32::RED, "Invalid hex");
                    }

                    ui.separator();
                    let ready = d.path.is_some() && metadata.is_some();
                    ui.horizontal(|ui| {
                        if ui.add_enabled(ready, egui::Button::new("Upload")).clicked() {
                            start = Some((d.conn_id, d.path.clone().unwrap(), metadata.unwrap()));
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
        }
        if let Some((conn_id, path, metadata)) = start {
            self.upload_seq += 1;
            let _ = self.cmd_tx.send(Command::UploadObject {
                conn_id,
                upload_id: self.upload_seq,
                path,
                metadata,
            });
            self.status = "Uploading…".to_string();
            self.upload_dialog = None;
        }
        if close {
            self.upload_dialog = None;
        }
    }

    /// Renders the metadata view/edit dialog (if open) and applies any save.
    pub(crate) fn metadata_dialog(&mut self, ctx: &egui::Context) {
        let mut save: Option<(i64, String, Vec<u8>)> = None;
        let mut closed: Vec<u64> = Vec::new();
        for view in &mut self.metadata_dialog {
            // A separate OS window (viewport) so it isn't clamped to the main
            // window. Each open view gets its own viewport id so several can show
            // at once.
            let builder = egui::ViewportBuilder::default()
                .with_title("Object details")
                .with_inner_size([600.0, 560.0]);
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("metadata-dialog", view.id)),
                builder,
                |vctx, _class| {
                    egui::CentralPanel::default().show(vctx, |ui| {
                        ui.label(egui::RichText::new(&view.object_id).monospace());
                        ui.label(format!("{} bytes", view.bytes.len()));
                        ui.separator();

                        if view.editing {
                            // Switch representation, converting the buffer so no edits
                            // are lost. Text→hex always works; hex→text only for valid
                            // UTF-8.
                            ui.horizontal(|ui| {
                                if ui.selectable_label(!view.edit_hex, "Text/JSON").clicked()
                                    && view.edit_hex
                                    && let Some(bytes) = parse_hex_edit(&view.edit_text)
                                    && let Some(text) = pretty_json(&bytes)
                                        .or_else(|| String::from_utf8(bytes).ok())
                                {
                                    view.edit_text = text;
                                    view.edit_hex = false;
                                }
                                if ui.selectable_label(view.edit_hex, "Hex").clicked()
                                    && !view.edit_hex
                                {
                                    let bytes = encode_metadata(&view.edit_text);
                                    view.edit_text = bytes_to_hex_edit(&bytes);
                                    view.edit_hex = true;
                                }
                            });

                            // Bound the editor height so the status line and the
                            // Save/Cancel row below always stay visible.
                            let editor_height = (ui.available_height() - 80.0).max(100.0);
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .max_height(editor_height)
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut view.edit_text)
                                            .code_editor()
                                            .desired_width(f32::INFINITY),
                                    );
                                });
                            // Encode the edit back to bytes; None means invalid input.
                            let encoded = if view.edit_hex {
                                parse_hex_edit(&view.edit_text)
                            } else {
                                Some(encode_metadata(&view.edit_text))
                            };
                            if view.edit_hex {
                                match &encoded {
                                    Some(b) => {
                                        ui.weak(format!("{} bytes of hex", b.len()));
                                    }
                                    None => {
                                        ui.colored_label(egui::Color32::RED, "Invalid hex");
                                    }
                                }
                            } else if serde_json::from_str::<serde_json::Value>(&view.edit_text)
                                .is_ok()
                            {
                                ui.weak("Valid JSON — will be saved compact.");
                            }
                            ui.separator();
                            let changed = encoded.as_ref().is_some_and(|b| *b != view.bytes);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(changed, egui::Button::new("Save")).clicked() {
                                    let bytes = encoded.unwrap();
                                    save =
                                        Some((view.conn_id, view.object_id.clone(), bytes.clone()));
                                    view.bytes = bytes;
                                    view.editing = false;
                                }
                                if ui.button("Cancel").clicked() {
                                    view.editing = false;
                                }
                                if changed {
                                    ui.weak("• unsaved changes");
                                }
                            });
                        } else {
                            // Pretty-print JSON; else show text; else a hex dump.
                            let pretty = pretty_json(&view.bytes);
                            let as_text = std::str::from_utf8(&view.bytes).ok();
                            let body = pretty
                                .clone()
                                .or_else(|| as_text.map(str::to_string))
                                .unwrap_or_else(|| hex_dump(&view.bytes));
                            if pretty.is_some() {
                                ui.weak("JSON");
                            }
                            // Metadata takes the upper part; slabs the rest below.
                            let body_height = (ui.available_height() * 0.4).clamp(80.0, 240.0);
                            egui::ScrollArea::both()
                                .id_salt(("md-body", view.id))
                                .auto_shrink([false, false])
                                .max_height(body_height)
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(body).monospace())
                                            .selectable(true),
                                    );
                                });
                            ui.separator();
                            if let Some(text) = as_text {
                                if ui.button("Edit").clicked() {
                                    view.edit_text = pretty.unwrap_or_else(|| text.to_string());
                                    view.edit_hex = false;
                                    view.editing = true;
                                }
                            } else if ui.button("Edit as hex").clicked() {
                                view.edit_text = bytes_to_hex_edit(&view.bytes);
                                view.edit_hex = true;
                                view.editing = true;
                            }

                            // The object's full slab/sector structure.
                            ui.separator();
                            match &view.slabs {
                                Some(slabs) if !slabs.is_empty() => {
                                    ui.label(
                                        egui::RichText::new(format!("Slabs ({})", slabs.len()))
                                            .strong(),
                                    );
                                    egui::ScrollArea::vertical()
                                        .id_salt(("md-slabs", view.id))
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| slabs_view(ui, view.id, slabs));
                                }
                                Some(_) => {
                                    ui.weak("No slabs (empty object).");
                                }
                                None => {
                                    ui.weak("Loading slab & sector structure…");
                                }
                            }
                        }
                    });
                    if vctx.input(|i| i.viewport().close_requested()) {
                        closed.push(view.id);
                    }
                },
            );
        }
        if let Some((conn_id, object_id, metadata)) = save {
            self.status = format!("Updating metadata for {object_id}…");
            let _ = self.cmd_tx.send(Command::UpdateMetadata {
                conn_id,
                object_id,
                metadata,
            });
        }
        if !closed.is_empty() {
            self.metadata_dialog.retain(|v| !closed.contains(&v.id));
        }
    }

    /// Requests an object's slab/sector structure once, deduping repeat requests
    /// via `slab_fetch_pending` (auto-expand re-runs every frame).
    pub(crate) fn request_slabs(&mut self, conn_id: i64, object_id: String) {
        if self.slabs_by_object.contains_key(&object_id)
            || self.slab_fetch_pending.contains(&object_id)
        {
            return;
        }
        self.slab_fetch_pending.insert(object_id.clone());
        let _ = self
            .cmd_tx
            .send(Command::FetchObjectStructure { conn_id, object_id });
    }

    /// Renders the objects table. `filtered`, when `Some`, lists the indices of
    /// `objects` to show; when `None`, all are shown. `selected` is the id to
    /// highlight; `sort` drives the header arrows. Expanded objects/slabs unfold
    /// into indented slab and sector rows, using `slabs_by_object` for the data.
    /// `id_query` (lowercased) is highlighted in any id it matches; with
    /// `scroll_to_match`, the first matching row is scrolled into view.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn objects_table(
        ui: &mut egui::Ui,
        objects: &[ObjectView],
        filtered: Option<&[usize]>,
        selected: Option<&str>,
        sort: (SortCol, SortDir),
        expanded_objects: &HashSet<String>,
        expanded_slabs: &HashSet<(String, usize)>,
        slabs_by_object: &HashMap<String, Vec<SlabView>>,
        id_query: &str,
        scroll_to_match: bool,
    ) -> TableResult {
        let (sort_col, sort_dir) = sort;
        let action = std::cell::Cell::new(None);
        let clicked = std::cell::Cell::new(None);
        // Set once we've scrolled the first matching row into view this pass.
        let first_match = std::cell::Cell::new(None::<()>);
        // A hash copied to the clipboard this frame (for a "copied" toast).
        let copied = std::cell::Cell::new(None::<String>);
        let sort_click = std::cell::Cell::new(None);
        let toggle_object = std::cell::Cell::new(None);
        let toggle_slab = std::cell::Cell::new(None);

        // Flatten the page into visual rows: each expanded object is followed by
        // its slab rows, and each expanded slab by its sector rows.
        let mut vis: Vec<Vis> = Vec::new();
        let page: Vec<usize> = match filtered {
            Some(f) => f.to_vec(),
            None => (0..objects.len()).collect(),
        };
        for &oi in &page {
            vis.push(Vis::Object(oi));
            let id = &objects[oi].id;
            if !expanded_objects.contains(id) {
                continue;
            }
            match slabs_by_object.get(id) {
                Some(slabs) if !slabs.is_empty() => {
                    vis.push(Vis::SlabHeader);
                    for (si, slab) in slabs.iter().enumerate() {
                        vis.push(Vis::Slab(oi, si));
                        if expanded_slabs.contains(&(id.clone(), si)) {
                            vis.push(Vis::SectorHeader);
                            for xi in 0..slab.sectors.len() {
                                vis.push(Vis::Sector(oi, si, xi));
                            }
                        }
                    }
                }
                Some(_) => vis.push(Vis::Empty),
                None => vis.push(Vis::Loading),
            }
        }

        // Width that fits a full 64-char hex id (plus cell padding), so the id
        // column shows the whole id by default.
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let id_width = ui.ctx().fonts_mut(|f| {
            f.layout_no_wrap("0".repeat(64), mono.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        }) + 12.0;
        // Colours for substring-match highlighting.
        let text_color = ui.visuals().text_color();
        let weak_color = ui.visuals().weak_text_color();
        // Green hover border, matching the hosts list.
        let hover_stroke = ui.visuals().widgets.hovered.bg_stroke;

        // Text-selectable labels swallow clicks over cell text; disable so a
        // click anywhere on a row selects the row.
        ui.style_mut().interaction.selectable_labels = false;

        // Full row width, so row guides can be painted across all columns from
        // within the first cell (captured before `ui` moves).
        let table_width = ui.available_width();

        TableBuilder::new(ui)
            .striped(false)
            .resizable(true)
            .sense(egui::Sense::click()) // row responds to click (select) and right-click (menu)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(id_width).at_least(120.0).clip(true)) // object id
            .column(Column::auto().at_least(70.0)) // size
            .column(Column::auto().at_least(50.0)) // slabs
            .column(Column::auto().at_least(140.0)) // updated
            .column(Column::remainder().at_least(160.0).clip(true)) // metadata
            .min_scrolled_height(0.0)
            .auto_shrink([false, false])
            .header(22.0, |mut header| {
                // A clickable, arrow-annotated header for a sortable column.
                let sort_header = |ui: &mut egui::Ui, label: &str, col: SortCol| {
                    let arrow = if sort_col == col {
                        sort_dir.arrow()
                    } else {
                        ""
                    };
                    let text = egui::RichText::new(format!("{label}{arrow}")).strong();
                    if ui.add(egui::Button::new(text).frame(false)).clicked() {
                        sort_click.set(Some(col));
                    }
                };
                // Numeric columns are right-aligned so digits line up.
                let right = egui::Layout::right_to_left(egui::Align::Center);
                header.col(|ui| {
                    ui.strong("Object ID");
                });
                header.col(|ui| {
                    ui.with_layout(right, |ui| sort_header(ui, "Size", SortCol::Size));
                });
                header.col(|ui| {
                    ui.with_layout(right, |ui| ui.strong("Slabs"));
                });
                header.col(|ui| {
                    sort_header(ui, "Updated", SortCol::Updated);
                });
                header.col(|ui| {
                    ui.strong("Metadata");
                });
            })
            .body(|body| {
                let right = egui::Layout::right_to_left(egui::Align::Center);
                // A frameless disclosure triangle; returns whether it was clicked.
                let triangle = |ui: &mut egui::Ui, open: bool| {
                    let sym = if open { "▼" } else { "▶" };
                    ui.add(egui::Button::new(egui::RichText::new(sym).small()).frame(false))
                        .clicked()
                };
                // A muted column label for the slab/sector sub-header rows.
                let hcell = |ui: &mut egui::Ui, text: &str| {
                    ui.label(egui::RichText::new(text).small().strong().weak());
                };
                // A truncated monospace hash that highlights any `id_query` match
                // and copies on click (table labels aren't text-selectable).
                // `width` bounds it when set.
                let copyable = |ui: &mut egui::Ui, text: &str, weak: bool, width: Option<f32>| {
                    let base = if weak { weak_color } else { text_color };
                    let job = highlight_job(text, id_query, mono.clone(), base, ACCENT);
                    let label = egui::Label::new(job).sense(egui::Sense::click());
                    let resp = match width {
                        Some(w) => ui.add_sized([w, 18.0], label),
                        None => ui.add(label),
                    };
                    if resp.clicked() {
                        ui.ctx().copy_text(text.to_owned());
                        copied.set(Some(text.to_owned()));
                    }
                    resp.on_hover_text(format!("{text}\n(click to copy)"))
                };
                let empty_cols = |row: &mut egui_extras::TableRow, n: usize| {
                    for _ in 0..n {
                        row.col(|_| {});
                    }
                };
                // No row backgrounds: per-depth indent guides and a separator under
                // the sub-headers carry the hierarchy. Painted from within the
                // first cell, before the cell's content.
                let paint_row = |ui: &mut egui::Ui, depth: u8, separator: bool| {
                    let cell = ui.max_rect();
                    let row = egui::Rect::from_min_size(
                        egui::pos2(cell.left(), cell.top()),
                        egui::vec2(table_width, cell.height()),
                    );
                    // The layer painter (unlike the cell painter) isn't clipped to
                    // the first column, so the separator can span the whole row.
                    let p = ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(row);
                    let guide = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(20));
                    for k in 1..=depth {
                        let x = cell.left() + 7.0 + (k as f32 - 1.0) * 16.0;
                        p.vline(x, row.y_range(), guide);
                    }
                    if separator {
                        p.hline(
                            row.x_range(),
                            row.bottom() - 0.5,
                            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(14)),
                        );
                    }
                };
                body.rows(20.0, vis.len(), |mut row| match vis[row.index()] {
                    Vis::Object(oi) => {
                        let obj = &objects[oi];
                        let is_selected = selected == Some(obj.id.as_str());
                        row.set_selected(is_selected);
                        row.col(|ui| {
                            ui.add_space(2.0);
                            if obj.slabs > 0 {
                                if triangle(ui, expanded_objects.contains(&obj.id)) {
                                    toggle_object.set(Some(oi));
                                }
                            } else {
                                ui.add_space(16.0);
                            }
                            let job =
                                highlight_job(&obj.id, id_query, mono.clone(), text_color, ACCENT);
                            let resp = ui.add(egui::Label::new(job)).on_hover_text(&obj.id);
                            if scroll_to_match
                                && !id_query.is_empty()
                                && obj.id.contains(id_query)
                                && first_match.get().is_none()
                            {
                                first_match.set(Some(()));
                                resp.scroll_to_me(Some(egui::Align::Center));
                            }
                        });
                        row.col(|ui| {
                            ui.with_layout(right, |ui| ui.monospace(human_size(obj.size)));
                        });
                        row.col(|ui| {
                            ui.with_layout(right, |ui| ui.monospace(obj.slabs.to_string()));
                        });
                        row.col(|ui| {
                            ui.monospace(short_time(&obj.updated_at));
                        });
                        row.col(|ui| {
                            if obj.metadata.is_empty() {
                                ui.weak("—");
                            } else {
                                ui.add(
                                    egui::Label::new(metadata_preview(&obj.metadata)).truncate(),
                                )
                                .on_hover_text("Double-click row to view metadata");
                            }
                        });
                        let resp = row.response();
                        // Green hover border (matching the hosts list), drawn after
                        // the cells so it sits over egui_extras' grey hover fill.
                        if resp.hovered() && !is_selected {
                            resp.ctx.layer_painter(resp.layer_id).rect_stroke(
                                resp.rect,
                                2.0,
                                hover_stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                        if resp.clicked() {
                            clicked.set(Some(oi));
                        }
                        if resp.double_clicked() {
                            action.set(Some(RowAction::ViewMetadata(oi)));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("View metadata").clicked() {
                                action.set(Some(RowAction::ViewMetadata(oi)));
                                ui.close();
                            }
                            // Text/JSON edits as text; binary edits as hex.
                            if ui.button("Edit metadata").clicked() {
                                action.set(Some(RowAction::EditMetadata(oi)));
                                ui.close();
                            }
                            if ui.button("Copy object id").clicked() {
                                ui.ctx().copy_text(objects[oi].id.clone());
                                ui.close();
                            }
                            if ui.button("⬇ Download").clicked() {
                                action.set(Some(RowAction::Download(oi)));
                                ui.close();
                            }
                            if ui.button("🗑 Delete").clicked() {
                                action.set(Some(RowAction::Delete(oi)));
                                ui.close();
                            }
                        });
                    }
                    Vis::SlabHeader => {
                        row.col(|ui| {
                            paint_row(ui, 1, true);
                            ui.add_space(18.0);
                            hcell(ui, "Slab");
                        });
                        row.col(|ui| {
                            ui.with_layout(right, |ui| hcell(ui, "Size"));
                        });
                        row.col(|ui| {
                            ui.with_layout(right, |ui| hcell(ui, "Redundancy"));
                        });
                        row.col(|ui| {
                            ui.with_layout(right, |ui| hcell(ui, "Version"));
                        });
                        row.col(|ui| hcell(ui, "Slab id"));
                    }
                    Vis::Slab(oi, si) => {
                        let slab = &slabs_by_object[&objects[oi].id][si];
                        row.col(|ui| {
                            paint_row(ui, 1, false);
                            ui.add_space(18.0);
                            if !slab.sectors.is_empty() {
                                let open = expanded_slabs.contains(&(objects[oi].id.clone(), si));
                                if triangle(ui, open) {
                                    toggle_slab.set(Some((oi, si)));
                                }
                            } else {
                                ui.add_space(16.0);
                            }
                            ui.monospace(format!("Slab {si}"));
                        });
                        row.col(|ui| {
                            ui.with_layout(right, |ui| {
                                ui.monospace(human_size(slab.length as u64))
                            });
                        });
                        row.col(|ui| {
                            ui.with_layout(right, |ui| {
                                ui.monospace(format!(
                                    "{} of {}",
                                    slab.min_shards,
                                    slab.sectors.len()
                                ))
                            });
                        });
                        row.col(|ui| {
                            ui.with_layout(right, |ui| ui.monospace(format!("v{}", slab.version)));
                        });
                        row.col(|ui| {
                            let resp = copyable(ui, &slab.id, false, None);
                            if scroll_to_match
                                && !id_query.is_empty()
                                && slab.id.contains(id_query)
                                && first_match.get().is_none()
                            {
                                first_match.set(Some(()));
                                resp.scroll_to_me(Some(egui::Align::Center));
                            }
                        });
                    }
                    Vis::SectorHeader => {
                        // Root in the id column, host in the metadata column: two
                        // wide, independently resizable columns.
                        row.col(|ui| {
                            paint_row(ui, 2, true);
                            ui.add_space(34.0);
                            hcell(ui, "Sector root");
                        });
                        empty_cols(&mut row, 3);
                        row.col(|ui| hcell(ui, "Host"));
                    }
                    Vis::Sector(oi, si, xi) => {
                        let sector = &slabs_by_object[&objects[oi].id][si].sectors[xi];
                        row.col(|ui| {
                            paint_row(ui, 2, false);
                            ui.add_space(34.0);
                            let resp = copyable(ui, &sector.root, false, None);
                            if scroll_to_match
                                && !id_query.is_empty()
                                && sector.root.contains(id_query)
                                && first_match.get().is_none()
                            {
                                first_match.set(Some(()));
                                resp.scroll_to_me(Some(egui::Align::Center));
                            }
                        });
                        empty_cols(&mut row, 3);
                        row.col(|ui| {
                            copyable(ui, &sector.host_key, false, None);
                        });
                    }
                    Vis::Loading => {
                        row.col(|ui| {
                            paint_row(ui, 1, false);
                            ui.add_space(18.0);
                            ui.weak("loading slabs…");
                        });
                        empty_cols(&mut row, 4);
                    }
                    Vis::Empty => {
                        row.col(|ui| {
                            paint_row(ui, 1, false);
                            ui.add_space(18.0);
                            ui.weak("no slabs (empty object)");
                        });
                        empty_cols(&mut row, 4);
                    }
                });
            });
        TableResult {
            action: action.get(),
            clicked: clicked.get(),
            sort: sort_click.get(),
            toggle_object: toggle_object.get(),
            toggle_slab: toggle_slab.get(),
            matched_scrolled: first_match.get().is_some(),
            copied: copied.take(),
        }
    }
}

/// A row in the flattened objects tree.
#[derive(Clone, Copy)]
enum Vis {
    Object(usize),
    /// Column labels shown once above an expanded object's slab rows.
    SlabHeader,
    Slab(usize, usize),
    /// Column labels shown once above an expanded slab's sector rows.
    SectorHeader,
    Sector(usize, usize, usize),
    /// The object is expanded but its slabs haven't loaded yet.
    Loading,
    /// The object is expanded but has no slabs.
    Empty,
}

/// What the user did in the objects table this frame.
pub(crate) struct TableResult {
    pub(crate) action: Option<RowAction>,
    pub(crate) clicked: Option<usize>,
    pub(crate) sort: Option<SortCol>,
    /// An object row whose expander was toggled (index into `objects`).
    pub(crate) toggle_object: Option<usize>,
    /// A slab row whose expander was toggled: (object index, slab index).
    pub(crate) toggle_slab: Option<(usize, usize)>,
    /// Whether a matching row was scrolled into view this frame.
    pub(crate) matched_scrolled: bool,
    /// A hash copied to the clipboard this frame (to show a "copied" toast).
    pub(crate) copied: Option<String>,
}

/// An action triggered on a row, carrying the object's original index.
#[derive(Clone, Copy)]
pub(crate) enum RowAction {
    ViewMetadata(usize),
    EditMetadata(usize),
    Download(usize),
    Delete(usize),
}

impl RowAction {
    pub(crate) fn index(self) -> usize {
        match self {
            RowAction::ViewMetadata(i)
            | RowAction::EditMetadata(i)
            | RowAction::Download(i)
            | RowAction::Delete(i) => i,
        }
    }
}

/// A row action resolved to owned object data, handled after the panel closure.
pub(crate) enum ResolvedAction {
    ViewMetadata {
        conn_id: i64,
        object_id: String,
        bytes: Vec<u8>,
        edit: bool,
    },
    Download {
        conn_id: i64,
        object_id: String,
    },
    Delete {
        conn_id: i64,
        object_id: String,
    },
}

/// State for the "upload object" dialog.
pub(crate) struct UploadDialog {
    pub(crate) conn_id: i64,
    pub(crate) path: Option<PathBuf>,
    /// When true, the metadata box is interpreted as hex; otherwise text/JSON.
    pub(crate) hex: bool,
    pub(crate) metadata_text: String,
}

/// The object whose full metadata is shown (and optionally edited) in the dialog.
pub(crate) struct MetadataView {
    /// Unique per window, so multiple views get distinct viewport ids.
    pub(crate) id: u64,
    pub(crate) conn_id: i64,
    pub(crate) object_id: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) editing: bool,
    /// When true, `edit_text` is a hex representation of the bytes (for binary
    /// metadata that isn't valid UTF-8); otherwise it's text/JSON.
    pub(crate) edit_hex: bool,
    pub(crate) edit_text: String,
    /// The object's slab/sector structure, `None` until it loads.
    pub(crate) slabs: Option<Vec<SlabView>>,
}

/// Builds a single-line, truncating layout of `text`, drawing each
/// (case-insensitive) occurrence of the lowercased `query` in `accent`. An empty
/// query yields the plain text.
fn highlight_job(
    text: &str,
    query: &str,
    font: egui::FontId,
    base: egui::Color32,
    accent: egui::Color32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
            ..Default::default()
        },
        ..Default::default()
    };
    let base_fmt = TextFormat {
        font_id: font.clone(),
        color: base,
        ..Default::default()
    };
    if query.is_empty() {
        job.append(text, 0.0, base_fmt);
        return job;
    }
    let hl_fmt = TextFormat {
        font_id: font,
        color: accent,
        ..Default::default()
    };
    let lower = text.to_ascii_lowercase();
    let mut i = 0;
    while let Some(rel) = lower[i..].find(query) {
        let s = i + rel;
        let e = s + query.len();
        if s > i {
            job.append(&text[i..s], 0.0, base_fmt.clone());
        }
        job.append(&text[s..e], 0.0, hl_fmt.clone());
        i = e;
    }
    if i < text.len() {
        job.append(&text[i..], 0.0, base_fmt);
    }
    job
}

/// A compact single-line preview of metadata bytes for the table cell: valid
/// UTF-8 shown inline (whitespace collapsed, truncated), else a byte-count marker.
fn metadata_preview(bytes: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return format!("<{} bytes>", bytes.len());
    };
    const MAX: usize = 300;
    let one_line: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let trimmed = one_line.trim();
    if trimmed.chars().count() > MAX {
        trimmed.chars().take(MAX).collect::<String>() + "…"
    } else {
        trimmed.to_string()
    }
}

/// If `bytes` is valid JSON, returns it pretty-printed; otherwise `None`.
pub(crate) fn pretty_json(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

/// Encodes edited text for storage: compact JSON when it parses as JSON,
/// otherwise the raw text bytes.
fn encode_metadata(text: &str) -> Vec<u8> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => serde_json::to_vec(&value).unwrap_or_else(|_| text.as_bytes().to_vec()),
        Err(_) => text.as_bytes().to_vec(),
    }
}

/// An editable hex representation of bytes: space-separated pairs, 16 per line.
pub(crate) fn bytes_to_hex_edit(bytes: &[u8]) -> String {
    bytes
        .chunks(16)
        .map(|chunk| {
            chunk
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parses hex text (whitespace ignored) back into bytes, or `None` if invalid.
fn parse_hex_edit(text: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    digits
        .chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

/// Formats bytes as a classic hex dump: `offset  hex bytes  ascii`.
fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in bytes.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!(
            "{:08x}  {:<47}  {}\n",
            i * 16,
            hex.join(" "),
            ascii
        ));
    }
    out
}

/// Renders an object's slabs and their sectors (each a Merkle root on a host).
fn slabs_view(ui: &mut egui::Ui, view_id: u64, slabs: &[SlabView]) {
    // Long hashes are truncated with an ellipsis rather than wrapping.
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    for (i, slab) in slabs.iter().enumerate() {
        egui::CollapsingHeader::new(format!(
            "Slab {i} · {} · {} sectors",
            human_size(slab.length as u64),
            slab.sectors.len(),
        ))
        .id_salt((view_id, "slab", i))
        .show(ui, |ui| {
            ui.label(format!(
                "v{} · min shards {} · offset {} · length {}",
                slab.version, slab.min_shards, slab.offset, slab.length
            ));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("id  {}", slab.id))
                        .monospace()
                        .weak(),
                )
                .truncate(),
            )
            .on_hover_text(&slab.id);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("key {}", slab.encryption_key))
                        .monospace()
                        .weak(),
                )
                .truncate(),
            )
            .on_hover_text(&slab.encryption_key);
            ui.add_space(2.0);
            for (j, sec) in slab.sectors.iter().enumerate() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{j:>2}. {} @ {}", sec.root, sec.host_key))
                            .monospace(),
                    )
                    .truncate(),
                )
                .on_hover_text(format!("root: {}\nhost: {}", sec.root, sec.host_key));
            }
        });
    }
}
