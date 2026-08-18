//! SQLite persistence. Each backend task opens its own [`Store`] (connection) to
//! the same DB file; SQLite (in WAL mode) handles concurrent access.
//!
//! Keys are stored in plaintext for now — a deliberate, temporary dev decision.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use sia_storage::{DateTime, Hash256, Object, ObjectEvent, ObjectsCursor, Utc};

/// A persisted, previously-approved connection.
pub struct StoredConnection {
    pub id: i64,
    pub nickname: String,
    pub app_id: String,
    pub indexer_url: String,
    pub app_key: [u8; 32],
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// The database schema, applied on every open. No migrations yet.
    const INIT_SQL: &'static str = include_str!("init.sql");

    /// Opens (creating if needed) the DB at `path` and applies the schema.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening database at {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(Self::INIT_SQL)
            .context("applying database schema")?;
        Ok(Self { conn })
    }

    /// Inserts (or updates by nickname) a connection row, returning its id.
    /// Nickname is the identity, so one app can be added under several nicknames.
    pub fn upsert_connection(
        &self,
        nickname: &str,
        app_id: &str,
        indexer_url: &str,
        account_key: &str,
        app_key: &[u8; 32],
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO connections
                (nickname, app_id, indexer_url, account_key, app_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(nickname) DO UPDATE SET
                app_id = excluded.app_id,
                indexer_url = excluded.indexer_url,
                account_key = excluded.account_key,
                app_key = excluded.app_key",
            rusqlite::params![
                nickname,
                app_id,
                indexer_url,
                account_key,
                app_key,
                Utc::now().to_rfc3339()
            ],
        )?;
        let id = self.conn.query_row(
            "SELECT id FROM connections WHERE nickname = ?1",
            [nickname],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// Lists all persisted connections for auto-reconnect on startup.
    pub fn list_connections(&self) -> Result<Vec<StoredConnection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, nickname, app_id, indexer_url, app_key FROM connections ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            let key_blob: Vec<u8> = row.get(4)?;
            let mut app_key = [0u8; 32];
            if key_blob.len() == 32 {
                app_key.copy_from_slice(&key_blob);
            }
            Ok(StoredConnection {
                id: row.get(0)?,
                nickname: row.get(1)?,
                app_id: row.get(2)?,
                indexer_url: row.get(3)?,
                app_key,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Deletes a connection; its `objects` and `sync_state` rows go with it via
    /// `ON DELETE CASCADE` (enforcement is enabled per-connection in
    /// [`Store::open`]).
    pub fn delete_connection(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM connections WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Upserts a single object and its full slab/sector structure after a local
    /// upload. Leaves the sync cursor alone so the regular sync still reconciles
    /// it later.
    pub fn upsert_object(
        &self,
        connection_id: i64,
        object: &Object,
        updated_at: &str,
    ) -> Result<()> {
        let object_id = object.id().to_string();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO objects
                (connection_id, object_id, deleted, updated_at, size, slab_count, metadata)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6)
             ON CONFLICT(connection_id, object_id) DO UPDATE SET
                deleted = 0,
                updated_at = excluded.updated_at,
                size = excluded.size,
                slab_count = excluded.slab_count,
                metadata = excluded.metadata",
            rusqlite::params![
                connection_id,
                object_id,
                updated_at,
                object.size() as i64,
                object.slabs().len() as i64,
                object.metadata,
            ],
        )?;
        Self::write_object_slabs(&tx, connection_id, &object_id, object)?;
        tx.commit()?;
        Ok(())
    }

    /// Updates a synced object's stored metadata (after it is updated on the
    /// indexer).
    pub fn set_object_metadata(
        &self,
        connection_id: i64,
        object_id: &str,
        metadata: &[u8],
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE objects SET metadata = ?3 WHERE connection_id = ?1 AND object_id = ?2",
            rusqlite::params![connection_id, object_id, metadata],
        )?;
        Ok(())
    }

    /// Deletes a single synced object row (after it is deleted on the indexer).
    pub fn delete_object_row(&self, connection_id: i64, object_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM objects WHERE connection_id = ?1 AND object_id = ?2",
            rusqlite::params![connection_id, object_id],
        )?;
        Ok(())
    }

    /// Whether a connection row still exists; the sync loop uses this to stop
    /// itself after deletion.
    pub fn connection_exists(&self, id: i64) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM connections WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Loads the sync cursor for a connection, if any.
    pub fn load_cursor(&self, connection_id: i64) -> Result<Option<ObjectsCursor>> {
        let row: Option<(Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT cursor_after, cursor_id FROM sync_state WHERE connection_id = ?1",
                [connection_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((Some(after), Some(id))) = row else {
            return Ok(None);
        };
        let after = DateTime::parse_from_rfc3339(&after)
            .context("parsing cursor timestamp")?
            .with_timezone(&Utc);
        let id = parse_hash(&id)?;
        Ok(Some(ObjectsCursor { after, id }))
    }

    fn save_cursor(
        conn: &Connection,
        connection_id: i64,
        after: DateTime<Utc>,
        id: &Hash256,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO sync_state (connection_id, cursor_after, cursor_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(connection_id) DO UPDATE SET
                cursor_after = excluded.cursor_after,
                cursor_id = excluded.cursor_id",
            rusqlite::params![connection_id, after.to_rfc3339(), id.to_string()],
        )?;
        Ok(())
    }

    /// Upserts a batch of events and advances the cursor to the last one.
    /// Returns how many events added, updated, or deleted an object.
    pub fn apply_events(&self, connection_id: i64, events: &[ObjectEvent]) -> Result<SyncCounts> {
        let mut counts = SyncCounts::default();
        // One transaction + one prepared statement for the whole page: a page can
        // be hundreds of rows, and per-row auto-commit fsyncs dominate otherwise.
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut exists = tx.prepare_cached(
                "SELECT 1 FROM objects WHERE connection_id = ?1 AND object_id = ?2",
            )?;
            let mut upsert = tx.prepare_cached(
                "INSERT INTO objects
                    (connection_id, object_id, deleted, updated_at, size, slab_count, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(connection_id, object_id) DO UPDATE SET
                    deleted = excluded.deleted,
                    updated_at = excluded.updated_at,
                    size = COALESCE(excluded.size, objects.size),
                    slab_count = COALESCE(excluded.slab_count, objects.slab_count),
                    metadata = COALESCE(excluded.metadata, objects.metadata)",
            )?;
            for event in events {
                let object_id = event.id.to_string();
                let kind = if event.deleted {
                    counts.deleted += 1;
                    "deleted"
                } else if exists.exists(rusqlite::params![connection_id, &object_id])? {
                    counts.updated += 1;
                    "updated"
                } else {
                    counts.added += 1;
                    "added"
                };
                log::debug!("sync[{connection_id}] {kind} object {object_id}");

                // Delete events carry no object, so size/metadata stay NULL and
                // COALESCE preserves any previously stored values.
                let (size, slab_count, metadata) = match &event.object {
                    Some(obj) => (
                        Some(obj.size() as i64),
                        Some(obj.slabs().len() as i64),
                        Some(obj.metadata.clone()),
                    ),
                    None => (None, None, None),
                };
                upsert.execute(rusqlite::params![
                    connection_id,
                    object_id,
                    event.deleted as i64,
                    event.updated_at.to_rfc3339(),
                    size,
                    slab_count,
                    metadata,
                ])?;

                // Persist the full slab/sector structure for present objects.
                if let Some(obj) = &event.object {
                    Self::write_object_slabs(&tx, connection_id, &object_id, obj)?;
                }
            }
        }
        if let Some(last) = events.last() {
            Self::save_cursor(&tx, connection_id, last.updated_at, &last.id)?;
        }
        tx.commit()?;
        Ok(counts)
    }

    /// Returns the non-deleted objects for a connection, newest first.
    pub fn objects(&self, connection_id: i64) -> Result<Vec<ObjectRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT object_id, COALESCE(size, 0), COALESCE(slab_count, 0), updated_at, metadata
             FROM objects
             WHERE connection_id = ?1 AND deleted = 0
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([connection_id], |row| {
            Ok(ObjectRow {
                id: row.get(0)?,
                size: row.get(1)?,
                slab_count: row.get(2)?,
                updated_at: row.get(3)?,
                metadata: row.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Computes aggregate stats for a connection: object/slab/sector totals and
    /// the sectors held per host (descending).
    pub fn stats(&self, connection_id: i64) -> Result<StatsRow> {
        let (object_count, total_size): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size), 0)
             FROM objects WHERE connection_id = ?1 AND deleted = 0",
            [connection_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (slab_count, total_min_shards): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(min_shards), 0)
             FROM object_slabs WHERE connection_id = ?1",
            [connection_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (sector_count, distinct_hosts): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COUNT(DISTINCT host_key)
             FROM object_sectors WHERE connection_id = ?1",
            [connection_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        // Bytes a host stores ≈ Σ (each sector's shard size = slab length / data
        // shards). Joins sectors to their slab for the shard size.
        let by_host = {
            let mut stmt = self.conn.prepare(
                "SELECT sec.host_key, COUNT(*),
                        CAST(COALESCE(SUM(CAST(sl.byte_length AS REAL) / sl.min_shards), 0)
                             AS INTEGER) bytes
                 FROM object_sectors sec
                 JOIN object_slabs sl
                   ON sl.connection_id = sec.connection_id
                  AND sl.object_id = sec.object_id
                  AND sl.slab_index = sec.slab_index
                 WHERE sec.connection_id = ?1
                 GROUP BY sec.host_key
                 ORDER BY bytes DESC",
            )?;
            let rows =
                stmt.query_map([connection_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        Ok(StatsRow {
            object_count,
            total_size,
            slab_count,
            sector_count,
            distinct_hosts,
            total_min_shards,
            by_host,
        })
    }

    /// Loads the full slab/sector structure for a single object, ordered.
    pub fn object_slabs(&self, connection_id: i64, object_id: &str) -> Result<Vec<SlabRow>> {
        let mut slabs: Vec<SlabRow> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT COALESCE(slab_id, ''), version, encryption_key, min_shards,
                        byte_offset, byte_length
                 FROM object_slabs
                 WHERE connection_id = ?1 AND object_id = ?2
                 ORDER BY slab_index",
            )?;
            let rows = stmt.query_map(rusqlite::params![connection_id, object_id], |row| {
                Ok(SlabRow {
                    id: row.get(0)?,
                    version: row.get(1)?,
                    encryption_key: row.get(2)?,
                    min_shards: row.get(3)?,
                    offset: row.get(4)?,
                    length: row.get(5)?,
                    sectors: Vec::new(),
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        // Attach sectors to their slabs (both are stored in index order).
        let mut stmt = self.conn.prepare_cached(
            "SELECT slab_index, root, host_key
             FROM object_sectors
             WHERE connection_id = ?1 AND object_id = ?2
             ORDER BY slab_index, sector_index",
        )?;
        let rows = stmt.query_map(rusqlite::params![connection_id, object_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (slab_index, root, host_key) = row?;
            if let Some(slab) = slabs.get_mut(slab_index as usize) {
                slab.sectors.push(SectorRow { root, host_key });
            }
        }
        Ok(slabs)
    }

    /// Returns the ids of a connection's objects with a slab id or sector root
    /// exactly equal to `query`. Equality on the indexed columns, so it stays fast
    /// on large accounts.
    pub fn objects_with_component(&self, connection_id: i64, query: &str) -> Result<Vec<String>> {
        let q = query.trim().to_lowercase(); // ids are stored lowercase hex
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT object_id FROM object_slabs
                WHERE connection_id = ?1 AND slab_id = ?2
             UNION
             SELECT object_id FROM object_sectors
                WHERE connection_id = ?1 AND root = ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![connection_id, q], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Rewrites an object's slabs and sectors. Sectors cascade-delete with their
    /// slabs, so only the slabs are cleared first. Takes an explicit connection
    /// so it can run inside a transaction.
    fn write_object_slabs(
        conn: &Connection,
        connection_id: i64,
        object_id: &str,
        object: &Object,
    ) -> Result<()> {
        conn.prepare_cached(
            "DELETE FROM object_slabs WHERE connection_id = ?1 AND object_id = ?2",
        )?
        .execute(rusqlite::params![connection_id, object_id])?;
        let mut ins_slab = conn.prepare_cached(
            "INSERT INTO object_slabs
                (connection_id, object_id, slab_index, slab_id, version, encryption_key,
                 min_shards, byte_offset, byte_length)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut ins_sector = conn.prepare_cached(
            "INSERT INTO object_sectors
                (connection_id, object_id, slab_index, sector_index, root, host_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (si, slab) in object.slabs().iter().enumerate() {
            ins_slab.execute(rusqlite::params![
                connection_id,
                object_id,
                si as i64,
                slab.digest().to_string(),
                u8::from(slab.version) as i64,
                to_hex(slab.encryption_key.as_ref()),
                slab.min_shards as i64,
                slab.offset as i64,
                slab.length as i64,
            ])?;
            for (xi, sector) in slab.sectors.iter().enumerate() {
                ins_sector.execute(rusqlite::params![
                    connection_id,
                    object_id,
                    si as i64,
                    xi as i64,
                    sector.root.to_string(),
                    sector.host_key.to_string(),
                ])?;
            }
        }
        Ok(())
    }
}

/// Lowercase hex encoding of a byte slice.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A synced object as stored, for display in the objects table.
pub struct ObjectRow {
    pub id: String,
    pub size: i64,
    pub slab_count: i64,
    pub updated_at: String,
    pub metadata: Vec<u8>,
}

/// A slab of an object, with its sectors, for display.
pub struct SlabRow {
    pub id: String,
    pub version: i64,
    pub encryption_key: String,
    pub min_shards: i64,
    pub offset: i64,
    pub length: i64,
    pub sectors: Vec<SectorRow>,
}

/// A single sector (shard) of a slab: a Merkle root stored on a host.
pub struct SectorRow {
    pub root: String,
    pub host_key: String,
}

/// Aggregate stats for a connection (for the Data tab).
pub struct StatsRow {
    pub object_count: i64,
    pub total_size: i64,
    pub slab_count: i64,
    pub sector_count: i64,
    pub distinct_hosts: i64,
    pub total_min_shards: i64,
    /// Per host: (public key, sector count, approx bytes stored), by bytes desc.
    pub by_host: Vec<(String, i64, i64)>,
}

/// How a batch of events changed the store.
#[derive(Default)]
pub struct SyncCounts {
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
}

fn parse_hash(s: &str) -> Result<Hash256> {
    s.trim().parse().context("parsing hash256 hex")
}
