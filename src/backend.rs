//! The async backend: owns the tokio runtime, the live [`Sdk`]s, and the
//! per-connection sync loops. It talks to the UI over channels.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use sia_storage::{
    Account, AppKey, Builder, DownloadOptions, Hash256, Host, HostQuery, Object, Sdk,
    ShardProgress, UploadOptions, Utc,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::apps::{app_metadata, parse_app_id};
use crate::store::Store;

/// Live SDKs by connection id, so command handlers can reach a connection
/// without going through its sync loop.
type SdkMap = Arc<Mutex<HashMap<i64, Sdk>>>;

/// How many events to request per sync page.
const SYNC_PAGE: usize = 500;
/// How long to wait after catching up before polling again.
const SYNC_IDLE: Duration = Duration::from_secs(30);
/// How long to wait after a sync error before retrying.
const SYNC_RETRY: Duration = Duration::from_secs(60);

/// Commands sent from the UI to the backend.
pub enum Command {
    /// Connect via the interactive approval flow (derives a key from the
    /// recovery phrase). Lands on an app's existing account only if both the
    /// connect key and phrase match its original registration.
    Connect {
        nickname: String,
        app_id: String,
        indexer_url: String,
        mnemonic: String,
    },
    /// Remove a connection and its synced data locally; leaves the indexer
    /// account untouched.
    RemoveConnection { id: i64 },
    /// Upload a file with the given metadata, then pin it. `upload_id` is
    /// UI-assigned so the progress window can track this upload.
    UploadObject {
        conn_id: i64,
        upload_id: u64,
        path: PathBuf,
        metadata: Vec<u8>,
    },
    /// Delete an object from the indexer.
    DeleteObject { conn_id: i64, object_id: String },
    /// Download an object's data to `dest`.
    DownloadObject {
        conn_id: i64,
        object_id: String,
        dest: PathBuf,
    },
    /// Re-fetch account limits/usage for a connection.
    RefreshAccount { conn_id: i64 },
    /// Re-fetch the usable host list for a connection.
    RefreshHosts { conn_id: i64 },
    /// Fetch a host's settings/prices from siascan (by public key).
    FetchHostDetail { public_key: String },
    /// Replace an object's metadata on the indexer.
    UpdateMetadata {
        conn_id: i64,
        object_id: String,
        metadata: Vec<u8>,
    },
    /// Load an object's stored slab/sector structure for display.
    FetchObjectStructure { conn_id: i64, object_id: String },
    /// Find objects containing a slab id or sector root matching `query`.
    FindObjectsByComponent { conn_id: i64, query: String },
    /// Compute aggregate stats for a connection (Data tab).
    FetchStats { conn_id: i64 },
}

/// Events sent from the backend to the UI.
pub enum Event {
    /// Global status text for the in-progress add-connection flow.
    Status(String),
    /// Approval URL for the in-progress add-connection flow.
    ApprovalUrl(String),
    /// A connection is live (freshly approved or reconnected on startup).
    ConnectionUp {
        id: i64,
        nickname: String,
        app_id: String,
    },
    /// Refreshed account info for a connection.
    Account {
        id: i64,
        account: AccountInfo,
    },
    /// The usable host list for a connection.
    Hosts {
        id: i64,
        hosts: Vec<HostInfo>,
    },
    /// Settings/prices for a host; `None` if unavailable.
    HostDetail {
        public_key: String,
        detail: Option<HostDetailInfo>,
    },
    /// Progress/result of an object download.
    Download(DownloadProgress),
    /// Progress/result of an object upload.
    Upload(UploadProgress),
    /// The current object rows for a connection.
    Objects {
        id: i64,
        objects: Vec<ObjectView>,
    },
    /// An object's slab/sector structure (in response to FetchObjectStructure).
    ObjectStructure {
        conn_id: i64,
        object_id: String,
        slabs: Vec<SlabView>,
    },
    /// Object ids matching a slab/sector-id search (FindObjectsByComponent).
    ComponentMatches {
        conn_id: i64,
        query: String,
        object_ids: Vec<String>,
    },
    /// Aggregate stats for a connection (FetchStats).
    Stats {
        conn_id: i64,
        stats: StatsInfo,
    },
    /// A connection was removed.
    ConnectionRemoved {
        id: i64,
    },
    Error(String),
}

/// Account limits/usage for display in the UI.
#[derive(Clone)]
pub struct AccountInfo {
    pub account_key: String,
    pub max_pinned_data: u64,
    pub remaining_storage: u64,
    pub pinned_data: u64,
    pub pinned_size: u64,
    pub ready: bool,
    pub app_name: String,
    pub last_used: String,
}

fn account_info(a: &Account) -> AccountInfo {
    AccountInfo {
        account_key: a.account_key.to_string(),
        max_pinned_data: a.max_pinned_data,
        remaining_storage: a.remaining_storage,
        pinned_data: a.pinned_data,
        pinned_size: a.pinned_size,
        ready: a.ready,
        app_name: a.app.name.clone(),
        last_used: a.last_used.to_rfc3339(),
    }
}

/// A usable host for display in the UI.
#[derive(Clone)]
pub struct HostInfo {
    pub public_key: String,
    pub addresses: Vec<String>,
    pub country: String,
    pub latitude: f64,
    pub longitude: f64,
    pub good_for_upload: bool,
}

fn host_info(h: &Host) -> HostInfo {
    HostInfo {
        public_key: h.public_key.to_string(),
        addresses: h
            .addresses
            .iter()
            .map(|a| format!("{} [{:?}]", a.address, a.protocol))
            .collect(),
        country: h.country_code.clone(),
        latitude: h.latitude,
        longitude: h.longitude,
        good_for_upload: h.good_for_upload,
    }
}

/// siascan's public API, used for host settings/pricing and the SC→USD rate.
const SIASCAN_API: &str = "https://api.siascan.com";

/// A host's RHP4 settings and prices (converted to USD) from siascan.
#[derive(Clone)]
pub struct HostDetailInfo {
    pub accepting_contracts: bool,
    pub protocol_version: String,
    pub release: String,
    pub total_storage_tb: f64,
    pub remaining_storage_tb: f64,
    pub max_contract_duration_days: f64,
    pub storage_usd_tb_month: f64,
    pub ingress_usd_tb: f64,
    pub egress_usd_tb: f64,
    pub contract_usd: f64,
    pub collateral_usd_tb_month: f64,
}

/// Fetches a host's settings/prices from siascan, converting prices to USD via
/// the cached exchange rate. `Ok(None)` if siascan has no record.
async fn fetch_host_detail(
    http: &reqwest::Client,
    rate_cache: &Mutex<Option<f64>>,
    pubkey: &str,
) -> Result<Option<HostDetailInfo>> {
    // Copy the rate out so the lock guard isn't held across the awaits below
    // (keeps the future Send).
    let cached_rate = *rate_cache.lock().unwrap();
    let rate = match cached_rate {
        Some(r) => r,
        None => {
            let r: f64 = http
                .get(format!("{SIASCAN_API}/exchange-rate/siacoin/usd"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            *rate_cache.lock().unwrap() = Some(r);
            r
        }
    };

    let resp = http
        .get(format!("{SIASCAN_API}/hosts/{pubkey}"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let host: serde_json::Value = resp.error_for_status()?.json().await?;
    Ok(Some(parse_host_detail(&host, rate)))
}

fn parse_host_detail(host: &serde_json::Value, rate: f64) -> HostDetailInfo {
    let v2 = &host["v2Settings"];
    let prices = &v2["prices"];
    // Currency values are decimal strings of hastings (1 SC = 1e24 hastings).
    let hastings = |v: &serde_json::Value| -> f64 {
        v.as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    let to_usd = |h: f64| h / 1e24 * rate;

    const TB: f64 = 1e12; // bytes
    const MONTH: f64 = 4320.0; // blocks (30 * 144)
    const SECTOR: f64 = 4.0 * 1024.0 * 1024.0; // 4 MiB

    let sectors = |v: &serde_json::Value| v.as_u64().unwrap_or(0) as f64 * SECTOR / TB;
    let version = if let Some(s) = v2["protocolVersion"].as_str() {
        s.to_string()
    } else if let Some(arr) = v2["protocolVersion"].as_array() {
        arr.iter()
            .filter_map(serde_json::Value::as_u64)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(".")
    } else {
        String::new()
    };

    HostDetailInfo {
        accepting_contracts: v2["acceptingContracts"].as_bool().unwrap_or(false),
        protocol_version: version,
        release: v2["release"].as_str().unwrap_or("").to_string(),
        total_storage_tb: sectors(&v2["totalStorage"]),
        remaining_storage_tb: sectors(&v2["remainingStorage"]),
        max_contract_duration_days: v2["maxContractDuration"].as_u64().unwrap_or(0) as f64 / 144.0,
        storage_usd_tb_month: to_usd(hastings(&prices["storagePrice"]) * TB * MONTH),
        ingress_usd_tb: to_usd(hastings(&prices["ingressPrice"]) * TB),
        egress_usd_tb: to_usd(hastings(&prices["egressPrice"]) * TB),
        contract_usd: to_usd(hastings(&prices["contractPrice"])),
        collateral_usd_tb_month: to_usd(hastings(&prices["collateral"]) * TB * MONTH),
    }
}

/// Fetches all usable hosts, paging through the indexer.
async fn fetch_host_infos(sdk: &Sdk) -> Result<Vec<HostInfo>> {
    const PAGE: u64 = 100;
    let mut hosts = Vec::new();
    for offset in (0..).step_by(PAGE as usize) {
        let page = sdk
            .hosts(HostQuery {
                offset: Some(offset),
                limit: Some(PAGE),
                ..Default::default()
            })
            .await
            .context("fetching hosts")?;
        let done = page.len() < PAGE as usize;
        hosts.extend(page.iter().map(host_info));
        if done {
            break;
        }
    }
    Ok(hosts)
}

/// Progress (or final state) of an object download, for the download window.
#[derive(Clone)]
pub struct DownloadProgress {
    pub object_id: String,
    pub dest: String,
    pub written: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
    /// Wall-clock seconds the download took; set only on the final `done` event.
    pub elapsed_secs: Option<f64>,
}

/// Progress/result of an object upload, for the uploads window.
pub struct UploadProgress {
    /// UI-assigned id used to key this upload's window entry.
    pub upload_id: u64,
    pub name: String,
    /// Encoded (data + parity) bytes uploaded so far, and the estimated total.
    pub written: u64,
    pub total: u64,
    /// Source file size, for the completion summary.
    pub size: u64,
    pub done: bool,
    /// The resulting object id, set only on the final `done` event.
    pub object_id: Option<String>,
    pub error: Option<String>,
    /// Wall-clock seconds the upload took; set only on the final `done` event.
    pub elapsed_secs: Option<f64>,
}

/// A row for the objects table in the UI.
pub struct ObjectView {
    pub id: String,
    pub size: u64,
    pub slabs: u64,
    pub updated_at: String,
    /// The object's raw (decrypted) metadata bytes.
    pub metadata: Vec<u8>,
}

/// A slab of an object (with its sectors), for the object details view.
#[derive(Clone)]
pub struct SlabView {
    pub id: String,
    pub version: i64,
    pub encryption_key: String,
    pub min_shards: i64,
    pub offset: i64,
    pub length: i64,
    pub sectors: Vec<SectorView>,
}

/// A single sector (shard): a Merkle root stored on a host.
#[derive(Clone)]
pub struct SectorView {
    pub root: String,
    pub host_key: String,
}

/// Aggregate stats for a connection, for the Data tab.
pub struct StatsInfo {
    pub object_count: i64,
    pub total_size: i64,
    pub slab_count: i64,
    pub sector_count: i64,
    pub distinct_hosts: i64,
    pub total_min_shards: i64,
    /// Per host: (public key, sector count, approx bytes stored), by bytes desc.
    pub by_host: Vec<(String, i64, i64)>,
}

impl From<crate::store::StatsRow> for StatsInfo {
    fn from(r: crate::store::StatsRow) -> Self {
        StatsInfo {
            object_count: r.object_count,
            total_size: r.total_size,
            slab_count: r.slab_count,
            sector_count: r.sector_count,
            distinct_hosts: r.distinct_hosts,
            total_min_shards: r.total_min_shards,
            by_host: r.by_host,
        }
    }
}

impl From<crate::store::SlabRow> for SlabView {
    fn from(r: crate::store::SlabRow) -> Self {
        SlabView {
            id: r.id,
            version: r.version,
            encryption_key: r.encryption_key,
            min_shards: r.min_shards,
            offset: r.offset,
            length: r.length,
            sectors: r
                .sectors
                .into_iter()
                .map(|s| SectorView {
                    root: s.root,
                    host_key: s.host_key,
                })
                .collect(),
        }
    }
}

/// Builds table rows from stored objects.
fn object_views(rows: Vec<crate::store::ObjectRow>) -> Vec<ObjectView> {
    rows.into_iter()
        .map(|r| ObjectView {
            id: r.id,
            size: r.size.max(0) as u64,
            slabs: r.slab_count.max(0) as u64,
            updated_at: r.updated_at,
            metadata: r.metadata,
        })
        .collect()
}

/// Sends events to the UI and wakes it up to repaint.
#[derive(Clone)]
struct Emitter {
    tx: Sender<Event>,
    ctx: eframe::egui::Context,
}

impl Emitter {
    fn send(&self, event: Event) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }

    fn status(&self, msg: impl Into<String>) {
        self.send(Event::Status(msg.into()));
    }

    fn error(&self, msg: impl Into<String>) {
        self.send(Event::Error(msg.into()));
    }

    fn account(&self, id: i64, account: &Account) {
        self.send(Event::Account {
            id,
            account: account_info(account),
        });
    }
}

/// Reads a connection's objects from the store and pushes them to the UI.
fn emit_objects(emitter: &Emitter, store: &Store, conn_id: i64) -> Result<()> {
    emitter.send(Event::Objects {
        id: conn_id,
        objects: object_views(store.objects(conn_id)?),
    });
    Ok(())
}

/// Spawns `op` on a connection's live SDK, reporting any error (or an inactive
/// connection) to the UI under `label`.
fn spawn_conn_op<F, Fut>(emitter: &Emitter, sdk: Option<Sdk>, label: &'static str, op: F)
where
    F: FnOnce(Sdk, Emitter) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send,
{
    let emitter = emitter.clone();
    tokio::spawn(async move {
        let Some(sdk) = sdk else {
            emitter.error(format!("{label} failed: connection is not active"));
            return;
        };
        if let Err(e) = op(sdk, emitter.clone()).await {
            log::error!("{label} failed: {e:#}");
            emitter.error(format!("{label} failed: {e:#}"));
        }
    });
}

/// Spawns the backend on a dedicated thread with its own tokio runtime,
/// returning the command sender and event receiver for the UI.
pub fn spawn(
    ctx: eframe::egui::Context,
    db_path: PathBuf,
) -> (UnboundedSender<Command>, Receiver<Event>) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let (event_tx, event_rx) = std::sync::mpsc::channel::<Event>();

    std::thread::Builder::new()
        .name("revelio-backend".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            rt.block_on(run(cmd_rx, Emitter { tx: event_tx, ctx }, db_path));
        })
        .expect("failed to spawn backend thread");

    (cmd_tx, event_rx)
}

async fn run(mut cmd_rx: UnboundedReceiver<Command>, emitter: Emitter, db_path: PathBuf) {
    let sdks: SdkMap = Arc::new(Mutex::new(HashMap::new()));
    let http = reqwest::Client::new();
    let usd_rate: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));

    // Reconnect any previously-approved connections and resume syncing them.
    if let Err(e) = reconnect_stored(&emitter, &db_path, sdks.clone()).await {
        log::warn!("startup reconnect failed: {e:#}");
    }

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::Connect {
                nickname,
                app_id,
                indexer_url,
                mnemonic,
            } => {
                let emitter = emitter.clone();
                let db_path = db_path.clone();
                let sdks = sdks.clone();
                tokio::spawn(async move {
                    if let Err(e) = connect(
                        &emitter,
                        &db_path,
                        sdks,
                        nickname,
                        app_id,
                        indexer_url,
                        mnemonic,
                    )
                    .await
                    {
                        log::error!("connect failed: {e:#}");
                        emitter.error(format!("Connect failed: {e:#}"));
                    }
                });
            }
            Command::RemoveConnection { id } => {
                // The connection's sync loop notices the deleted row and stops
                // itself on its next iteration.
                sdks.lock().unwrap().remove(&id);
                match Store::open(&db_path).and_then(|s| s.delete_connection(id)) {
                    Ok(()) => {
                        log::info!("removed connection {id}");
                        emitter.send(Event::ConnectionRemoved { id });
                    }
                    Err(e) => {
                        log::error!("failed to remove connection {id}: {e:#}");
                        emitter.error(format!("Failed to remove connection: {e:#}"));
                    }
                }
            }
            Command::UploadObject {
                conn_id,
                upload_id,
                path,
                metadata,
            } => {
                let sdk = sdks.lock().unwrap().get(&conn_id).cloned();
                let db_path = db_path.clone();
                spawn_conn_op(&emitter, sdk, "Upload", move |sdk, emitter| async move {
                    upload_object(&emitter, &db_path, sdk, conn_id, upload_id, path, metadata).await
                });
            }
            Command::DeleteObject { conn_id, object_id } => {
                let sdk = sdks.lock().unwrap().get(&conn_id).cloned();
                let db_path = db_path.clone();
                spawn_conn_op(&emitter, sdk, "Delete", move |sdk, emitter| async move {
                    delete_object(&emitter, &db_path, sdk, conn_id, object_id).await
                });
            }
            Command::DownloadObject {
                conn_id,
                object_id,
                dest,
            } => {
                let sdk = sdks.lock().unwrap().get(&conn_id).cloned();
                spawn_conn_op(&emitter, sdk, "Download", move |sdk, emitter| async move {
                    download_object(&emitter, sdk, object_id, dest).await
                });
            }
            Command::RefreshAccount { conn_id } => {
                let sdk = sdks.lock().unwrap().get(&conn_id).cloned();
                spawn_conn_op(
                    &emitter,
                    sdk,
                    "Account refresh",
                    move |sdk, emitter| async move {
                        let account = sdk.account().await.context("fetching account")?;
                        emitter.account(conn_id, &account);
                        Ok(())
                    },
                );
            }
            Command::RefreshHosts { conn_id } => {
                let sdk = sdks.lock().unwrap().get(&conn_id).cloned();
                spawn_conn_op(
                    &emitter,
                    sdk,
                    "Hosts refresh",
                    move |sdk, emitter| async move {
                        let hosts = fetch_host_infos(&sdk).await?;
                        emitter.send(Event::Hosts { id: conn_id, hosts });
                        Ok(())
                    },
                );
            }
            Command::FetchHostDetail { public_key } => {
                let emitter = emitter.clone();
                let http = http.clone();
                let usd_rate = usd_rate.clone();
                tokio::spawn(async move {
                    let detail = match fetch_host_detail(&http, &usd_rate, &public_key).await {
                        Ok(d) => d,
                        Err(e) => {
                            log::warn!("siascan host detail for {public_key} failed: {e:#}");
                            None
                        }
                    };
                    emitter.send(Event::HostDetail { public_key, detail });
                });
            }
            Command::UpdateMetadata {
                conn_id,
                object_id,
                metadata,
            } => {
                let sdk = sdks.lock().unwrap().get(&conn_id).cloned();
                let db_path = db_path.clone();
                spawn_conn_op(&emitter, sdk, "Update", move |sdk, emitter| async move {
                    update_metadata(&emitter, &db_path, sdk, conn_id, object_id, metadata).await
                });
            }
            Command::FetchObjectStructure { conn_id, object_id } => {
                let emitter = emitter.clone();
                let db_path = db_path.clone();
                tokio::spawn(async move {
                    match Store::open(&db_path).and_then(|s| s.object_slabs(conn_id, &object_id)) {
                        Ok(rows) => emitter.send(Event::ObjectStructure {
                            conn_id,
                            object_id,
                            slabs: rows.into_iter().map(SlabView::from).collect(),
                        }),
                        Err(e) => {
                            log::error!("loading object structure failed: {e:#}");
                            emitter.error(format!("Loading object structure failed: {e:#}"));
                        }
                    }
                });
            }
            Command::FindObjectsByComponent { conn_id, query } => {
                let emitter = emitter.clone();
                let db_path = db_path.clone();
                tokio::spawn(async move {
                    match Store::open(&db_path)
                        .and_then(|s| s.objects_with_component(conn_id, &query))
                    {
                        Ok(object_ids) => emitter.send(Event::ComponentMatches {
                            conn_id,
                            query,
                            object_ids,
                        }),
                        Err(e) => {
                            log::error!("component search failed: {e:#}");
                            emitter.error(format!("Component search failed: {e:#}"));
                        }
                    }
                });
            }
            Command::FetchStats { conn_id } => {
                let emitter = emitter.clone();
                let db_path = db_path.clone();
                tokio::spawn(async move {
                    match Store::open(&db_path).and_then(|s| s.stats(conn_id)) {
                        Ok(stats) => emitter.send(Event::Stats {
                            conn_id,
                            stats: stats.into(),
                        }),
                        Err(e) => {
                            log::error!("stats failed: {e:#}");
                            emitter.error(format!("Stats failed: {e:#}"));
                        }
                    }
                });
            }
        }
    }
}

/// Uploads a file's data with the given metadata, pins the object, then records
/// it locally and refreshes the UI.
async fn upload_object(
    emitter: &Emitter,
    db_path: &std::path::Path,
    sdk: Sdk,
    conn_id: i64,
    upload_id: u64,
    path: PathBuf,
    metadata: Vec<u8>,
) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    emitter.status(format!("Uploading {name}…"));

    let size = tokio::fs::metadata(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?
        .len();

    // Progress is reported per erasure-coded shard, so the expected total is the
    // source size inflated by parity overhead.
    let opts = UploadOptions::default();
    let (data, parity) = (opts.data_shards as u64, opts.parity_shards as u64);
    let total = size.saturating_mul(data + parity) / data.max(1);

    // The shard callback must be Send + Sync but our event Sender is not Sync,
    // so the callback only bumps an atomic counter; a poller task turns that into
    // progress events.
    let uploaded = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let opts = UploadOptions {
        shard_uploaded: Some({
            let uploaded = uploaded.clone();
            Arc::new(move |sp: ShardProgress| {
                uploaded.fetch_add(sp.shard_size as u64, Ordering::Relaxed);
            })
        }),
        ..opts
    };

    let emit = |written, done, object_id, error, elapsed_secs| {
        emitter.send(Event::Upload(UploadProgress {
            upload_id,
            name: name.clone(),
            written,
            total,
            size,
            done,
            object_id,
            error,
            elapsed_secs,
        }));
    };
    emit(0, false, None, None, None);

    let started = std::time::Instant::now();
    let poller = {
        let emitter = emitter.clone();
        let uploaded = uploaded.clone();
        let finished = finished.clone();
        let name = name.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if finished.load(Ordering::Relaxed) {
                    break;
                }
                emitter.send(Event::Upload(UploadProgress {
                    upload_id,
                    name: name.clone(),
                    written: uploaded.load(Ordering::Relaxed),
                    total,
                    size,
                    done: false,
                    object_id: None,
                    error: None,
                    elapsed_secs: None,
                }));
            }
        })
    };

    let result = async {
        let file = tokio::fs::File::open(&path)
            .await
            .with_context(|| format!("opening {}", path.display()))?;
        let object = Object::new(Some(metadata));
        let object = sdk.upload(object, file, opts).await.context("uploading")?;
        sdk.pin_object(&object).await.context("pinning object")?;

        let store = Store::open(db_path)?;
        store.upsert_object(conn_id, &object, &Utc::now().to_rfc3339())?;
        emit_objects(emitter, &store, conn_id)?;
        anyhow::Ok(object.id().to_string())
    }
    .await;

    // Stop the poller before emitting the final state so it can't overwrite it.
    finished.store(true, Ordering::Relaxed);
    let _ = poller.await;

    match result {
        Ok(object_id) => {
            emit(
                total,
                true,
                Some(object_id.clone()),
                None,
                Some(started.elapsed().as_secs_f64()),
            );
            emitter.status(format!("Uploaded {name} ({object_id})"));
        }
        // Report failures through the upload window so the entry shows the
        // error instead of vanishing.
        Err(e) => emit(0, false, None, Some(format!("{e:#}")), None),
    }
    Ok(())
}

/// Deletes an object on the indexer, then drops its local row and refreshes the
/// UI's object list.
async fn delete_object(
    emitter: &Emitter,
    db_path: &std::path::Path,
    sdk: Sdk,
    conn_id: i64,
    object_id: String,
) -> Result<()> {
    let key: Hash256 = object_id.parse().context("invalid object id")?;
    sdk.delete_object(&key).await.context("deleting object")?;

    let store = Store::open(db_path)?;
    store.delete_object_row(conn_id, &object_id)?;
    emit_objects(emitter, &store, conn_id)?;
    emitter.status(format!("Deleted object {object_id}"));
    Ok(())
}

/// Replaces an object's metadata on the indexer (id is unchanged since it
/// derives from slabs, not metadata), then updates the local row and UI.
async fn update_metadata(
    emitter: &Emitter,
    db_path: &std::path::Path,
    sdk: Sdk,
    conn_id: i64,
    object_id: String,
    metadata: Vec<u8>,
) -> Result<()> {
    let key: Hash256 = object_id.parse().context("invalid object id")?;
    let mut object = sdk.object(&key).await.context("fetching object")?;
    object.metadata = metadata.clone();
    sdk.update_object_metadata(&object)
        .await
        .context("updating metadata")?;

    let store = Store::open(db_path)?;
    store.set_object_metadata(conn_id, &object_id, &metadata)?;
    emit_objects(emitter, &store, conn_id)?;
    emitter.status(format!("Updated metadata for {object_id}"));
    Ok(())
}

/// Fetches and decrypts an object, streaming it to `dest` while emitting
/// progress. Failures are reported via a [`Download`](Event::Download) update
/// (so the download window shows them), hence this always returns `Ok`.
async fn download_object(
    emitter: &Emitter,
    sdk: Sdk,
    object_id: String,
    dest: PathBuf,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dest_str = dest.display().to_string();
    let update = |written, total, done, error, elapsed_secs| {
        emitter.send(Event::Download(DownloadProgress {
            object_id: object_id.clone(),
            dest: dest_str.clone(),
            written,
            total,
            done,
            error,
            elapsed_secs,
        }));
    };

    let started = std::time::Instant::now();
    let result = async {
        let key: Hash256 = object_id.parse().context("invalid object id")?;
        let object = sdk.object(&key).await.context("fetching object")?;
        let total = object.size();
        update(0, total, false, None, None);

        let mut reader = sdk
            .download(&object, DownloadOptions::default())
            .context("starting download")?;
        let mut file = tokio::fs::File::create(&dest)
            .await
            .with_context(|| format!("creating {}", dest.display()))?;

        let mut buf = vec![0u8; 1 << 20];
        let mut written = 0u64;
        let mut last_emit = 0u64;
        loop {
            let n = reader.read(&mut buf).await.context("reading")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).await.context("writing file")?;
            written += n as u64;
            if written - last_emit >= (4 << 20) {
                last_emit = written;
                update(written, total, false, None, None);
            }
        }
        file.flush().await.context("flushing file")?;
        anyhow::Ok((written, total))
    }
    .await;

    match result {
        Ok((written, total)) => update(
            written,
            total,
            true,
            None,
            Some(started.elapsed().as_secs_f64()),
        ),
        Err(e) => update(0, 0, false, Some(format!("{e:#}")), None),
    }
    Ok(())
}

/// Runs the approval flow, persists the derived key, and starts syncing.
async fn connect(
    emitter: &Emitter,
    db_path: &std::path::Path,
    sdks: SdkMap,
    nickname: String,
    app_id: String,
    indexer_url: String,
    mnemonic: String,
) -> Result<()> {
    let id = parse_app_id(&app_id)?;
    let meta = app_metadata(id);

    emitter.status("Requesting connection…");
    let builder = Builder::new(indexer_url.clone(), meta).context("creating builder")?;
    let requesting = builder
        .request_connection()
        .await
        .context("requesting connection")?;

    emitter.send(Event::ApprovalUrl(requesting.response_url().to_string()));
    emitter.status("Waiting for approval in your indexd account…");
    let approved = requesting
        .wait_for_approval()
        .await
        .context("waiting for approval")?;

    emitter.status("Registering…");
    let sdk = approved
        .register(&mnemonic)
        .await
        .context("registering app")?;

    let account = sdk.account().await.ok();
    let account_key = account
        .as_ref()
        .map(|a| a.account_key.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let app_key = sdk.app_key().export();
    let store = Store::open(db_path)?;
    let conn_id =
        store.upsert_connection(&nickname, &app_id, &indexer_url, &account_key, &app_key)?;

    emitter.send(Event::ConnectionUp {
        id: conn_id,
        nickname: nickname.clone(),
        app_id: app_id.clone(),
    });
    if let Some(a) = &account {
        emitter.account(conn_id, a);
    }
    if let Ok(hosts) = fetch_host_infos(&sdk).await {
        emitter.send(Event::Hosts { id: conn_id, hosts });
    }
    log::info!(
        "connected {nickname:?} (app {app_id}) as account {account_key} (connection {conn_id})"
    );

    sdks.lock().unwrap().insert(conn_id, sdk.clone());
    sync_loop(emitter.clone(), db_path.to_path_buf(), conn_id, sdk).await;
    Ok(())
}

/// Reconnects persisted connections using their stored app keys (no approval).
async fn reconnect_stored(
    emitter: &Emitter,
    db_path: &std::path::Path,
    sdks: SdkMap,
) -> Result<()> {
    let stored = {
        let store = Store::open(db_path)?;
        store.list_connections()?
    };

    // Populate the sidebar from the DB (no network) so cached connections and
    // objects show right away.
    for conn in &stored {
        emitter.send(Event::ConnectionUp {
            id: conn.id,
            nickname: conn.nickname.clone(),
            app_id: conn.app_id.clone(),
        });
        if let Ok(store) = Store::open(db_path) {
            let _ = emit_objects(emitter, &store, conn.id);
        }
    }

    // Then authenticate and sync each connection concurrently so slow ones
    // don't hold up the others.
    for conn in stored {
        let id = match parse_app_id(&conn.app_id) {
            Ok(id) => id,
            Err(e) => {
                log::warn!("skipping connection {}: {e:#}", conn.id);
                continue;
            }
        };
        let emitter = emitter.clone();
        let db_path = db_path.to_path_buf();
        let sdks = sdks.clone();
        tokio::spawn(async move {
            let builder = match Builder::new(conn.indexer_url.clone(), app_metadata(id)) {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("connection {}: {e:#}", conn.id);
                    return;
                }
            };
            match builder.connected(&AppKey::import(conn.app_key)).await {
                Ok(Some(sdk)) => {
                    if let Ok(a) = sdk.account().await {
                        emitter.account(conn.id, &a);
                    }
                    if let Ok(hosts) = fetch_host_infos(&sdk).await {
                        emitter.send(Event::Hosts { id: conn.id, hosts });
                    }
                    sdks.lock().unwrap().insert(conn.id, sdk.clone());
                    sync_loop(emitter, db_path, conn.id, sdk).await;
                }
                Ok(None) => log::warn!("connection {} is no longer authenticated", conn.id),
                Err(e) => log::warn!("reconnect of connection {} failed: {e:#}", conn.id),
            }
        });
    }
    Ok(())
}

/// Continuously pages object events into the store and pushes the id list to the
/// UI. Owns `sdk` so its background refresh tasks stay alive.
async fn sync_loop(emitter: Emitter, db_path: PathBuf, conn_id: i64, sdk: Sdk) {
    // One connection for the task's lifetime; retry until it opens.
    let store = loop {
        match Store::open(&db_path) {
            Ok(s) => break s,
            Err(e) => {
                emitter.error(format!("Database error: {e:#}"));
                tokio::time::sleep(SYNC_RETRY).await;
            }
        }
    };

    loop {
        // Stop syncing once the connection has been removed.
        match store.connection_exists(conn_id) {
            Ok(true) => {}
            Ok(false) => {
                log::info!("connection {conn_id} removed; stopping sync");
                return;
            }
            Err(e) => {
                emitter.error(format!("Database error: {e:#}"));
                tokio::time::sleep(SYNC_RETRY).await;
                continue;
            }
        }

        let cursor = match store.load_cursor(conn_id) {
            Ok(c) => c,
            Err(e) => {
                emitter.error(format!("Cursor error: {e:#}"));
                tokio::time::sleep(SYNC_RETRY).await;
                continue;
            }
        };

        match sdk.object_events(cursor, Some(SYNC_PAGE)).await {
            Ok(events) => {
                let count = events.len();
                if count > 0 {
                    // The connection may have been deleted during the fetch above;
                    // applying now would fail the objects→connections foreign key.
                    match store.connection_exists(conn_id) {
                        Ok(true) => {}
                        Ok(false) => {
                            log::info!("connection {conn_id} removed; stopping sync");
                            return;
                        }
                        Err(e) => {
                            emitter.error(format!("Database error: {e:#}"));
                            tokio::time::sleep(SYNC_RETRY).await;
                            continue;
                        }
                    }
                    let counts = match store.apply_events(conn_id, &events) {
                        Ok(c) => c,
                        Err(e) => {
                            // A concurrent delete races the check above and shows
                            // up as a foreign-key error; stop quietly in that case.
                            if !store.connection_exists(conn_id).unwrap_or(false) {
                                log::info!("connection {conn_id} removed; stopping sync");
                                return;
                            }
                            emitter.error(format!("Store error: {e:#}"));
                            tokio::time::sleep(SYNC_RETRY).await;
                            continue;
                        }
                    };
                    log::debug!(
                        "conn {conn_id}: applied {count} events ({} added, {} updated, {} deleted)",
                        counts.added,
                        counts.updated,
                        counts.deleted,
                    );
                    if let Err(e) = emit_objects(&emitter, &store, conn_id) {
                        emitter.error(format!("Read error: {e:#}"));
                    }
                }
                // A partial page means we're caught up: refresh account, then idle.
                // A full page means there may be more — loop immediately to drain.
                if count < SYNC_PAGE {
                    if let Ok(a) = sdk.account().await {
                        emitter.account(conn_id, &a);
                    }
                    tokio::time::sleep(SYNC_IDLE).await;
                }
            }
            Err(e) => {
                emitter.error(format!("Sync error: {e:#}"));
                tokio::time::sleep(SYNC_RETRY).await;
            }
        }
    }
}
