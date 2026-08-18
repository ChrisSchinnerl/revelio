//! The egui frontend. Renders a connections sidebar, a connect form, and a
//! per-connection table of synced objects.

use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

use crate::apps::{KNOWN_APPS, app_name_for};
use crate::backend::{
    self, AccountInfo, Command, DownloadProgress, Event, HostInfo, ObjectView, SlabView, StatsInfo,
    UploadProgress,
};

mod data;
mod format;
mod hosts;
mod objects;
mod transfers;

use format::*;
use hosts::*;
use objects::*;

/// Which tab of the selected connection is shown in the central panel.
#[derive(Clone, Copy, PartialEq, Default)]
enum Tab {
    #[default]
    Objects,
    Hosts,
    Data,
}

/// A live connection as tracked by the UI.
struct ConnState {
    id: i64,
    nickname: String,
    app_id: String,
    account: Option<AccountInfo>,
    /// When revelio last fetched the account from the indexer.
    account_refreshed: Option<Instant>,
    hosts: Option<Vec<HostInfo>>,
    /// When revelio last fetched the host list.
    hosts_refreshed: Option<Instant>,
    objects: Vec<ObjectView>,
    /// Aggregate stats for the Data tab, cached with its refresh time.
    stats: Option<StatsInfo>,
    stats_refreshed: Option<Instant>,
    /// A stats fetch is in flight (dedupes the auto-fetch on tab open).
    stats_pending: bool,
}

pub struct App {
    cmd_tx: UnboundedSender<Command>,
    event_rx: Receiver<Event>,

    // connect form
    show_connect: bool,
    nickname: String,
    app_choice: usize, // index into KNOWN_APPS, or == len for "Custom…"
    custom_app_id: String,
    indexer_url: String,
    mnemonic: String,

    // state driven by backend events
    connections: Vec<ConnState>,
    selected: Option<i64>,
    selected_object: Option<String>,
    pending_delete: Option<i64>,
    pending_object_delete: Option<(i64, String)>,
    upload_dialog: Option<UploadDialog>,
    /// Open metadata windows (each its own OS window); `metadata_view_seq`
    /// hands out a unique id per window.
    metadata_dialog: Vec<MetadataView>,
    metadata_view_seq: u64,
    filters: Filters,
    show_filters: bool,
    sort_col: SortCol,
    sort_dir: SortDir,
    view_cache: Option<ViewCache>,
    objects_epoch: u64,
    /// Expanded objects (by id) and slabs (by id + index); `slabs_by_object`
    /// caches expanded objects' structure, pruned on collapse.
    expanded_objects: HashSet<String>,
    expanded_slabs: HashSet<(String, usize)>,
    slabs_by_object: HashMap<String, Vec<SlabView>>,
    /// The DB-backed slab/sector-id search: the query and connection it ran
    /// for, and the matching object ids.
    component_query: String,
    component_conn: Option<i64>,
    component_matches: HashSet<String>,
    /// Objects whose structure has been requested but not yet received (dedup).
    slab_fetch_pending: HashSet<String>,
    /// Set when the id filter changes, so the first match is scrolled into view.
    search_scroll_pending: bool,
    /// When a hash was last copied, for the transient "Copied" toast.
    copied_at: Option<Instant>,
    tab: Tab,
    host_filter: String,
    host_good: GoodFilter,
    /// The host public key selected on the map / in the list (highlighted in both).
    selected_host: Option<String>,
    /// Set when a map dot is clicked, so the list scrolls that row into view once.
    scroll_to_selected: bool,
    /// Map view: zoom factor and the normalized `[0,1]` top-left of the visible
    /// region (`1.0, 0.0, 0.0` shows the whole world).
    map_zoom: f32,
    map_vx: f32,
    map_vy: f32,
    host_details: HashMap<String, HostDetail>,
    downloads: HashMap<String, DownloadProgress>,
    /// In-flight/finished uploads, keyed by their UI-assigned id.
    uploads: HashMap<u64, UploadProgress>,
    upload_seq: u64,
    page: usize,
    page_size: usize,
    connecting: bool,
    status: String,
    approval_url: Option<String>,
    /// An approval URL that should be opened in the browser this frame.
    open_url: Option<String>,
    error: Option<String>,
}

/// The brand accent green (matches the wordmark).
pub(crate) const ACCENT: egui::Color32 = egui::Color32::from_rgb(45, 200, 110);

/// Applies revelio's theme: green accent, rounded widgets, roomier spacing.
fn configure_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;

    // Brand-green accent in place of egui's default blue.
    v.selection.bg_fill = egui::Color32::from_rgb(26, 74, 48);
    v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.5, ACCENT);

    let r = egui::Rounding::same(4.0);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = r;
    }
    v.window_rounding = egui::Rounding::same(8.0);

    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);

    ctx.set_style(style);
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);
        let db_path = crate::db_path();
        log::info!("using database at {}", db_path.display());
        let (cmd_tx, event_rx) = backend::spawn(cc.egui_ctx.clone(), db_path);
        Self {
            cmd_tx,
            event_rx,
            show_connect: false,
            nickname: String::new(),
            app_choice: 0,
            custom_app_id: String::new(),
            indexer_url: "https://sia.storage".to_string(),
            mnemonic: String::new(),
            connections: Vec::new(),
            selected: None,
            selected_object: None,
            pending_delete: None,
            pending_object_delete: None,
            upload_dialog: None,
            metadata_dialog: Vec::new(),
            metadata_view_seq: 0,
            filters: Filters::default(),
            show_filters: false,
            sort_col: SortCol::default(),
            sort_dir: SortDir::default(),
            view_cache: None,
            objects_epoch: 0,
            expanded_objects: HashSet::new(),
            expanded_slabs: HashSet::new(),
            slabs_by_object: HashMap::new(),
            component_query: String::new(),
            component_conn: None,
            component_matches: HashSet::new(),
            slab_fetch_pending: HashSet::new(),
            search_scroll_pending: false,
            copied_at: None,
            tab: Tab::default(),
            host_filter: String::new(),
            host_good: GoodFilter::default(),
            selected_host: None,
            scroll_to_selected: false,
            map_zoom: 1.0,
            map_vx: 0.0,
            map_vy: 0.0,
            host_details: HashMap::new(),
            downloads: HashMap::new(),
            uploads: HashMap::new(),
            upload_seq: 0,
            page: 0,
            page_size: 100,
            connecting: false,
            status: String::new(),
            approval_url: None,
            open_url: None,
            error: None,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                Event::Status(s) => self.status = s,
                Event::ApprovalUrl(url) => {
                    self.approval_url = Some(url.clone());
                    self.open_url = Some(url); // auto-open the browser below
                }
                Event::ConnectionUp {
                    id,
                    nickname,
                    app_id,
                } => {
                    self.connecting = false;
                    self.show_connect = false;
                    self.approval_url = None;
                    self.mnemonic.clear();
                    self.nickname.clear();
                    self.status.clear();
                    if let Some(conn) = self.connections.iter_mut().find(|c| c.id == id) {
                        conn.nickname = nickname;
                        conn.app_id = app_id;
                    } else {
                        self.connections.push(ConnState {
                            id,
                            nickname,
                            app_id,
                            account: None,
                            account_refreshed: None,
                            hosts: None,
                            hosts_refreshed: None,
                            objects: Vec::new(),
                            stats: None,
                            stats_refreshed: None,
                            stats_pending: false,
                        });
                    }
                    if self.selected.is_none() {
                        self.selected = Some(id);
                    }
                }
                Event::Account { id, account } => {
                    if let Some(conn) = self.connections.iter_mut().find(|c| c.id == id) {
                        conn.account = Some(account);
                        conn.account_refreshed = Some(Instant::now());
                    }
                }
                Event::Hosts { id, hosts } => {
                    if let Some(conn) = self.connections.iter_mut().find(|c| c.id == id) {
                        conn.hosts = Some(hosts);
                        conn.hosts_refreshed = Some(Instant::now());
                    }
                }
                Event::Stats { conn_id, stats } => {
                    if let Some(conn) = self.connections.iter_mut().find(|c| c.id == conn_id) {
                        conn.stats = Some(stats);
                        conn.stats_refreshed = Some(Instant::now());
                        conn.stats_pending = false;
                    }
                }
                Event::HostDetail { public_key, detail } => {
                    let state = match detail {
                        Some(info) => HostDetail::Ready(Box::new(info)),
                        None => HostDetail::Missing,
                    };
                    self.host_details.insert(public_key, state);
                }
                Event::Download(p) => {
                    self.downloads.insert(p.object_id.clone(), p);
                }
                Event::Upload(p) => {
                    self.uploads.insert(p.upload_id, p);
                }
                Event::ObjectStructure {
                    conn_id,
                    object_id,
                    slabs,
                } => {
                    // Fill any open detail window(s) waiting on this object.
                    for view in self
                        .metadata_dialog
                        .iter_mut()
                        .filter(|v| v.conn_id == conn_id && v.object_id == object_id)
                    {
                        view.slabs = Some(slabs.clone());
                    }
                    // Cache for the table's inline tree; search-only entries
                    // are pruned when the id filter clears.
                    self.slab_fetch_pending.remove(&object_id);
                    self.slabs_by_object.insert(object_id, slabs);
                }
                Event::ComponentMatches {
                    conn_id,
                    query,
                    object_ids,
                } => {
                    // Ignore stale results (query or connection changed since).
                    if self.component_conn == Some(conn_id) && self.component_query == query {
                        self.component_matches = object_ids.into_iter().collect();
                        // Filter result depends on this; recompute the view.
                        self.view_cache = None;
                    }
                }
                Event::Objects { id, objects } => {
                    // A successful sync page clears any prior (often transient) error.
                    self.error = None;
                    if let Some(conn) = self.connections.iter_mut().find(|c| c.id == id) {
                        conn.objects = objects;
                        // Object set changed; invalidate the memoized filter result.
                        self.objects_epoch = self.objects_epoch.wrapping_add(1);
                    }
                }
                Event::ConnectionRemoved { id } => {
                    self.connections.retain(|c| c.id != id);
                    if self.selected == Some(id) {
                        self.selected = self.connections.first().map(|c| c.id);
                    }
                }
                Event::Error(e) => {
                    self.connecting = false;
                    self.error = Some(e);
                }
            }
        }
    }

    fn selected_app_id(&self) -> String {
        if self.app_choice < KNOWN_APPS.len() {
            KNOWN_APPS[self.app_choice].app_id.to_string()
        } else {
            self.custom_app_id.clone()
        }
    }

    fn connect_form(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("connect_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Nickname");
                ui.add(
                    egui::TextEdit::singleline(&mut self.nickname)
                        .hint_text("e.g. s3d-prod (must be unique)")
                        .desired_width(420.0),
                );
                ui.end_row();

                ui.label("App");
                let selected = if self.app_choice < KNOWN_APPS.len() {
                    KNOWN_APPS[self.app_choice].name
                } else {
                    "Custom…"
                };
                egui::ComboBox::from_id_salt("app_choice")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (i, app) in KNOWN_APPS.iter().enumerate() {
                            ui.selectable_value(&mut self.app_choice, i, app.name);
                        }
                        ui.selectable_value(&mut self.app_choice, KNOWN_APPS.len(), "Custom…");
                    });
                ui.end_row();

                if self.app_choice >= KNOWN_APPS.len() {
                    ui.label("App id (hex)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.custom_app_id)
                            .hint_text("64 hex characters")
                            .desired_width(420.0),
                    );
                    ui.end_row();
                }

                ui.label("Indexer URL");
                ui.add(egui::TextEdit::singleline(&mut self.indexer_url).desired_width(420.0));
                ui.end_row();

                ui.label("Recovery phrase");
                ui.add(
                    egui::TextEdit::multiline(&mut self.mnemonic)
                        .hint_text("12-word recovery phrase")
                        .desired_rows(2)
                        .desired_width(420.0),
                );
                ui.end_row();
            });

        ui.add_space(8.0);

        let nickname_taken = self
            .connections
            .iter()
            .any(|c| c.nickname == self.nickname.trim());
        let ready = !self.nickname.trim().is_empty()
            && !nickname_taken
            && !self.selected_app_id().trim().is_empty()
            && !self.indexer_url.trim().is_empty()
            && !self.mnemonic.trim().is_empty()
            && !self.connecting;

        if nickname_taken {
            ui.colored_label(egui::Color32::RED, "That nickname is already in use.");
        }

        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new("Connect"))
                .clicked()
            {
                self.error = None;
                self.approval_url = None;
                self.connecting = true;
                self.status = "Connecting…".to_string();
                let _ = self.cmd_tx.send(Command::Connect {
                    nickname: self.nickname.trim().to_string(),
                    app_id: self.selected_app_id().trim().to_string(),
                    indexer_url: self.indexer_url.trim().to_string(),
                    mnemonic: self.mnemonic.trim().to_string(),
                });
            }
            if ui.button("Cancel").clicked() {
                self.show_connect = false;
            }
            if self.connecting {
                ui.spinner();
            }
        });

        if let Some(url) = &self.approval_url {
            ui.add_space(8.0);
            ui.separator();
            ui.label("Your browser should open to approve this connection.");
            ui.label("If it didn't, open this link:");
            ui.hyperlink(url);
        }
    }

    /// Renders the account panel for the selected connection.
    fn account_panel(&self, ui: &mut egui::Ui, conn: &ConnState) {
        let Some(acc) = &conn.account else {
            ui.weak("Loading account…");
            return;
        };
        egui::Grid::new("account_info")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.label("Key");
                ui.add(
                    egui::Label::new(egui::RichText::new(&acc.account_key).monospace())
                        .truncate()
                        .selectable(true),
                )
                .on_hover_text(&acc.account_key);
                ui.end_row();

                ui.label("Status");
                if acc.ready {
                    ui.colored_label(egui::Color32::from_rgb(60, 160, 60), "ready");
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(200, 150, 40),
                        "not ready (propagating)",
                    );
                }
                ui.end_row();

                ui.label("Pinned data");
                ui.label(format!(
                    "{} / {}",
                    human_size(acc.pinned_data),
                    max_size(acc.max_pinned_data)
                ));
                ui.end_row();

                ui.label("Remaining");
                ui.label(max_size(acc.remaining_storage));
                ui.end_row();

                ui.label("On network");
                ui.label(human_size(acc.pinned_size))
                    .on_hover_text("Size stored across hosts, including redundancy");
                ui.end_row();

                ui.label("App");
                ui.label(&acc.app_name);
                ui.end_row();

                ui.label("Last used");
                ui.label(short_time(&acc.last_used));
                ui.end_row();
            });

        if is_limited(acc.max_pinned_data) && acc.max_pinned_data > 0 {
            let frac = (acc.pinned_data as f32 / acc.max_pinned_data as f32).clamp(0.0, 1.0);
            ui.add(
                egui::ProgressBar::new(frac).text(format!("{:.1}% of limit used", frac * 100.0)),
            );
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                let _ = self
                    .cmd_tx
                    .send(Command::RefreshAccount { conn_id: conn.id });
            }
            if let Some(t) = conn.account_refreshed {
                ui.weak(format!("refreshed {}", ago(t.elapsed())));
                // Keep the relative time ticking without waiting for another event.
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
        });
    }
}

/// Modal confirmation: `Some(true)` confirmed, `Some(false)` cancelled, `None`
/// while open. `code` is shown on its own monospace line (e.g. an object id).
fn confirm_modal(
    ctx: &egui::Context,
    title: &str,
    message: &str,
    code: Option<&str>,
    detail: &str,
) -> Option<bool> {
    let mut result = None;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(message);
            if let Some(code) = code {
                ui.label(egui::RichText::new(code).monospace());
            }
            ui.small(detail);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    result = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    result = Some(false);
                }
            });
        });
    result
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();

        // Auto-open the approval URL; the shown link is only a fallback.
        if let Some(url) = self.open_url.take() {
            ctx.open_url(egui::OpenUrl::new_tab(url));
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("revelio")
                        .monospace()
                        .size(30.0)
                        .strong()
                        .color(ACCENT),
                );
                if !self.status.is_empty() {
                    ui.separator();
                    ui.label(&self.status);
                }
            });
        });

        if self.show_connect {
            egui::Window::new("Add connection")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| self.connect_form(ui));
        }

        if let Some(id) = self.pending_delete {
            let nickname = self
                .connections
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.nickname.clone())
                .unwrap_or_default();
            // Removal is applied when the backend's ConnectionRemoved event arrives.
            match confirm_modal(
                ctx,
                "Delete connection",
                &format!("Delete connection \"{nickname}\"?"),
                None,
                "Removes locally synced objects for this connection. \
                 Your indexer account is not affected.",
            ) {
                Some(true) => {
                    let _ = self.cmd_tx.send(Command::RemoveConnection { id });
                    self.pending_delete = None;
                }
                Some(false) => self.pending_delete = None,
                None => {}
            }
        }

        egui::SidePanel::left("connections")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
                // Account info fills the lower half of the sidebar.
                egui::TopBottomPanel::bottom("account")
                    .resizable(true)
                    .default_height(300.0)
                    .show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        ui.heading("Account");
                        ui.separator();
                        let sel = self
                            .selected
                            .and_then(|id| self.connections.iter().position(|c| c.id == id));
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| match sel {
                                Some(cidx) => {
                                    let conn = &self.connections[cidx];
                                    self.account_panel(ui, conn);
                                }
                                None => {
                                    ui.weak("No connection selected.");
                                }
                            });
                    });

                // Connections list fills the upper half.
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    // Keep connection rows on a single line rather than wrapping.
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.heading("Connections");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("➕").on_hover_text("Add connection").clicked() {
                                self.show_connect = true;
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if self.connections.is_empty() {
                                ui.weak("None yet. Click ➕ to add one.");
                            }
                            let mut clicked_select = None;
                            let mut clicked_delete = None;
                            for conn in &self.connections {
                                let app = app_name_for(&conn.app_id).unwrap_or("custom");
                                let label = format!(
                                    "{}  ({}, {} objs)",
                                    conn.nickname,
                                    app,
                                    conn.objects.len()
                                );
                                let resp =
                                    ui.selectable_label(self.selected == Some(conn.id), label);
                                if resp.clicked() {
                                    clicked_select = Some(conn.id);
                                }
                                resp.context_menu(|ui| {
                                    if ui.button("🗑 Delete").clicked() {
                                        clicked_delete = Some(conn.id);
                                        ui.close_menu();
                                    }
                                });
                            }
                            if let Some(id) = clicked_select {
                                self.selected = Some(id);
                                self.tab = Tab::Objects; // objects tab by default
                            }
                            if let Some(id) = clicked_delete {
                                self.pending_delete = Some(id);
                            }
                        });
                });
            });

        let selected_cidx = self
            .selected
            .and_then(|id| self.connections.iter().position(|c| c.id == id));

        // The id filter also searches slab/sector ids via the DB. Re-run that
        // lookup whenever the query text or selected connection changes.
        if let Some(cidx) = selected_cidx {
            let conn_id = self.connections[cidx].id;
            let query = self.filters.id.trim().to_string();
            if query.is_empty() {
                if self.component_conn.is_some() {
                    self.component_conn = None;
                    self.component_query.clear();
                    self.component_matches.clear();
                    self.search_scroll_pending = false;
                    // Drop slabs cached only to reveal search matches.
                    self.slabs_by_object
                        .retain(|id, _| self.expanded_objects.contains(id));
                }
            } else if self.component_conn != Some(conn_id) || self.component_query != query {
                self.component_conn = Some(conn_id);
                self.component_query = query.clone();
                self.component_matches.clear(); // pending until the event arrives
                self.view_cache = None;
                self.page = 0; // jump to first page of matches
                self.search_scroll_pending = true;
                let _ = self
                    .cmd_tx
                    .send(Command::FindObjectsByComponent { conn_id, query });
            }
        }

        // Auto-expand objects matched via a slab/sector id, fetching their
        // structure so the matching rows are revealed.
        let id_query = self.filters.id.trim().to_lowercase();
        let mut eff_objects = self.expanded_objects.clone();
        let mut eff_slabs = self.expanded_slabs.clone();
        if let Some(cidx) = selected_cidx
            && self.tab == Tab::Objects
            && !id_query.is_empty()
            && !self.component_matches.is_empty()
            && self.component_matches.len() <= 50
        {
            let conn_id = self.connections[cidx].id;
            let matched: Vec<String> = self.component_matches.iter().cloned().collect();
            let mut to_fetch = Vec::new();
            for id in matched {
                eff_objects.insert(id.clone());
                if let Some(slabs) = self.slabs_by_object.get(&id) {
                    for (si, slab) in slabs.iter().enumerate() {
                        if slab.sectors.iter().any(|s| s.root.contains(&id_query)) {
                            eff_slabs.insert((id.clone(), si));
                        }
                    }
                } else {
                    to_fetch.push(id);
                }
            }
            for id in to_fetch {
                self.request_slabs(conn_id, id);
            }
        }

        let mut resolved_action: Option<ResolvedAction> = None;
        let mut new_selection: Option<String> = None;
        let mut toggle_object: Option<usize> = None;
        let mut toggle_slab: Option<(usize, usize)> = None;
        let scroll_to_match = self.search_scroll_pending;
        let mut matched_scrolled = false;
        let mut copied = false;
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = self.error.clone() {
                ui.horizontal(|ui| {
                    if ui.small_button("✕").on_hover_text("Dismiss").clicked() {
                        self.error = None;
                    }
                    ui.colored_label(egui::Color32::RED, err);
                });
                ui.add_space(4.0);
            }

            let Some(cidx) = selected_cidx else {
                ui.weak("Select or add a connection.");
                return;
            };

            // Scoped borrow so the filters form below can take `&mut self`.
            {
                let conn = &self.connections[cidx];
                let app = app_name_for(&conn.app_id).unwrap_or("custom");
                ui.heading(&conn.nickname);
                ui.label(format!("App: {app}  ({})", conn.app_id));
            }
            ui.add_space(4.0);

            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Objects,
                    egui::RichText::new("Objects").size(16.0),
                );
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Hosts,
                    egui::RichText::new("Hosts").size(16.0),
                );
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Data,
                    egui::RichText::new("Data").size(16.0),
                );
            });
            ui.separator();

            if self.tab == Tab::Hosts {
                self.hosts_view(ui, cidx);
                return;
            }
            if self.tab == Tab::Data {
                self.data_view(ui, cidx);
                return;
            }

            // A custom index order is needed when filtering, or when sorting
            // by anything other than the natural updated-descending order.
            let default_sort = self.sort_col == SortCol::Updated && self.sort_dir == SortDir::Desc;
            let ordered: Option<Vec<usize>> = if self.filters.is_active() || !default_sort {
                self.refresh_view_cache(cidx);
                Some(self.view_cache.as_ref().unwrap().indices.clone())
            } else {
                None
            };

            let conn = &self.connections[cidx];
            let conn_id = conn.id;
            let total = conn.objects.len();
            let matched = ordered.as_ref().map_or(total, |v| v.len());
            ui.horizontal(|ui| {
                ui.heading(if self.filters.is_active() {
                    format!("Objects ({matched} of {total})")
                } else {
                    format!("Objects ({total})")
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(egui::RichText::new("📤 Upload").size(15.0))
                        .on_hover_text("Upload a file to this connection")
                        .clicked()
                    {
                        self.upload_dialog = Some(UploadDialog {
                            conn_id,
                            path: None,
                            hex: false,
                            metadata_text: String::new(),
                        });
                    }
                    ui.toggle_value(&mut self.show_filters, "Filters");
                });
            });
            if self.show_filters {
                self.filters.form(ui);
            }

            // Keep the page in range, then render one page's worth.
            let page_count = matched.div_ceil(self.page_size).max(1);
            if self.page >= page_count {
                self.page = page_count - 1;
            }
            let start = self.page * self.page_size;
            let end = (start + self.page_size).min(matched);

            ui.horizontal(|ui| {
                ui.label("Rows/page:");
                egui::ComboBox::from_id_salt("page_size")
                    .selected_text(self.page_size.to_string())
                    .width(80.0)
                    .show_ui(ui, |ui| {
                        for n in [50usize, 100, 250, 500, 1000] {
                            ui.selectable_value(&mut self.page_size, n, n.to_string());
                        }
                    });
                ui.separator();
                if ui
                    .add_enabled(self.page > 0, egui::Button::new("◀ Prev"))
                    .clicked()
                {
                    self.page -= 1;
                }
                ui.label(format!("Page {} / {}", self.page + 1, page_count));
                if ui
                    .add_enabled(self.page + 1 < page_count, egui::Button::new("Next ▶"))
                    .clicked()
                {
                    self.page += 1;
                }
                let first = if matched == 0 { 0 } else { start + 1 };
                ui.weak(format!("showing {first}–{end} of {matched}"));
            });
            ui.separator();

            if conn.objects.is_empty() {
                ui.weak("No objects synced yet.");
                return;
            }

            let page_indices: Vec<usize> = match &ordered {
                Some(v) => v[start..end].to_vec(),
                None => (start..end).collect(),
            };
            let table = Self::objects_table(
                ui,
                &conn.objects,
                Some(&page_indices),
                self.selected_object.as_deref(),
                (self.sort_col, self.sort_dir),
                &eff_objects,
                &eff_slabs,
                &self.slabs_by_object,
                &id_query,
                scroll_to_match,
            );
            toggle_object = table.toggle_object;
            toggle_slab = table.toggle_slab;
            matched_scrolled = table.matched_scrolled;
            if table.copied.is_some() {
                copied = true;
            }
            if let Some(col) = table.sort {
                if self.sort_col == col {
                    self.sort_dir = match self.sort_dir {
                        SortDir::Asc => SortDir::Desc,
                        SortDir::Desc => SortDir::Asc,
                    };
                } else {
                    self.sort_col = col;
                    self.sort_dir = SortDir::Desc;
                }
                self.page = 0;
            }
            let (action, clicked) = (table.action, table.clicked);
            if let Some(i) = clicked
                && let Some(obj) = conn.objects.get(i)
            {
                new_selection = Some(obj.id.clone());
            }
            if let Some(action) = action
                && let Some(obj) = conn.objects.get(action.index())
            {
                resolved_action = Some(match action {
                    RowAction::ViewMetadata(_) => ResolvedAction::ViewMetadata {
                        conn_id: conn.id,
                        object_id: obj.id.clone(),
                        bytes: obj.metadata.clone(),
                        edit: false,
                    },
                    RowAction::EditMetadata(_) => ResolvedAction::ViewMetadata {
                        conn_id: conn.id,
                        object_id: obj.id.clone(),
                        bytes: obj.metadata.clone(),
                        edit: true,
                    },
                    RowAction::Download(_) => ResolvedAction::Download {
                        conn_id: conn.id,
                        object_id: obj.id.clone(),
                    },
                    RowAction::Delete(_) => ResolvedAction::Delete {
                        conn_id: conn.id,
                        object_id: obj.id.clone(),
                    },
                });
            }
        });

        if let Some(id) = new_selection {
            self.selected_object = Some(id);
        }
        // Stop scrolling once the first match is in view.
        if matched_scrolled {
            self.search_scroll_pending = false;
        }
        if copied {
            self.copied_at = Some(Instant::now());
        }

        // Collapse frees the object's cached slabs and any expanded slabs under
        // it; expand fetches the structure if not cached.
        if let Some(oi) = toggle_object
            && let Some(cidx) = selected_cidx
        {
            let conn_id = self.connections[cidx].id;
            if let Some(obj) = self.connections[cidx].objects.get(oi) {
                let id = obj.id.clone();
                if self.expanded_objects.remove(&id) {
                    self.slabs_by_object.remove(&id);
                    self.expanded_slabs.retain(|(oid, _)| oid != &id);
                } else {
                    self.expanded_objects.insert(id.clone());
                    self.request_slabs(conn_id, id);
                }
            }
        }
        // Toggle a slab row's sectors.
        if let Some((oi, si)) = toggle_slab
            && let Some(cidx) = selected_cidx
            && let Some(obj) = self.connections[cidx].objects.get(oi)
        {
            let key = (obj.id.clone(), si);
            if !self.expanded_slabs.remove(&key) {
                self.expanded_slabs.insert(key);
            }
        }

        match resolved_action {
            Some(ResolvedAction::ViewMetadata {
                conn_id,
                object_id,
                bytes,
                edit,
            }) => {
                // Prefill the editor: pretty JSON/text for UTF-8 metadata, hex
                // for raw bytes.
                let edit_hex = edit && std::str::from_utf8(&bytes).is_err();
                let edit_text = if !edit {
                    String::new()
                } else if edit_hex {
                    bytes_to_hex_edit(&bytes)
                } else {
                    pretty_json(&bytes)
                        .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned())
                };
                // Load the slab/sector structure for the window.
                let _ = self.cmd_tx.send(Command::FetchObjectStructure {
                    conn_id,
                    object_id: object_id.clone(),
                });
                self.metadata_view_seq += 1;
                self.metadata_dialog.push(MetadataView {
                    id: self.metadata_view_seq,
                    conn_id,
                    object_id,
                    bytes,
                    editing: edit,
                    edit_hex,
                    edit_text,
                    slabs: None,
                });
            }
            Some(ResolvedAction::Download { conn_id, object_id }) => {
                if let Some(dest) = rfd::FileDialog::new().set_file_name(&object_id).save_file() {
                    // Progress is shown in the downloads window.
                    let _ = self.cmd_tx.send(Command::DownloadObject {
                        conn_id,
                        object_id,
                        dest,
                    });
                }
            }
            Some(ResolvedAction::Delete { conn_id, object_id }) => {
                self.pending_object_delete = Some((conn_id, object_id));
            }
            None => {}
        }

        self.upload_dialog(ctx);
        self.downloads_window(ctx);
        self.uploads_window(ctx);
        self.metadata_dialog(ctx);

        if let Some((conn_id, object_id)) = self.pending_object_delete.clone() {
            match confirm_modal(
                ctx,
                "Delete object",
                "Delete this object?",
                Some(&object_id),
                "Removes it from the indexer. This cannot be undone.",
            ) {
                Some(true) => {
                    let _ = self
                        .cmd_tx
                        .send(Command::DeleteObject { conn_id, object_id });
                    self.pending_object_delete = None;
                }
                Some(false) => self.pending_object_delete = None,
                None => {}
            }
        }

        // Transient "Copied" toast.
        if let Some(t) = self.copied_at {
            if t.elapsed() < Duration::from_millis(1300) {
                egui::Area::new(egui::Id::new("copied-toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -28.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::popup(&ctx.style()).show(ui, |ui| {
                            ui.label(egui::RichText::new("✔ Copied to clipboard").color(ACCENT));
                        });
                    });
                ctx.request_repaint_after(Duration::from_millis(200));
            } else {
                self.copied_at = None;
            }
        }
    }
}
