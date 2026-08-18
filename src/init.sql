-- revelio database schema, run on every Store::open. No migrations yet.

CREATE TABLE IF NOT EXISTS connections (
    id          INTEGER PRIMARY KEY,
    nickname    TEXT NOT NULL,
    app_id      TEXT NOT NULL,
    indexer_url TEXT NOT NULL,
    account_key TEXT NOT NULL,
    app_key     BLOB NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE(nickname)
);

CREATE TABLE IF NOT EXISTS sync_state (
    connection_id INTEGER PRIMARY KEY REFERENCES connections(id) ON DELETE CASCADE,
    cursor_after  TEXT,
    cursor_id     TEXT
);

CREATE TABLE IF NOT EXISTS objects (
    connection_id INTEGER NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    object_id     TEXT NOT NULL,
    deleted       INTEGER NOT NULL,
    updated_at    TEXT NOT NULL,
    size          INTEGER,
    slab_count    INTEGER,
    metadata      BLOB,
    PRIMARY KEY (connection_id, object_id)
);

-- Backs `objects()`: filter by connection + deleted, sorted by updated_at.
CREATE INDEX IF NOT EXISTS idx_objects_listing
    ON objects (connection_id, deleted, updated_at DESC);

-- Object structure: an ordered list of erasure-coded slabs. Rewritten on
-- each (re)sync or upload.
CREATE TABLE IF NOT EXISTS object_slabs (
    connection_id  INTEGER NOT NULL,
    object_id      TEXT NOT NULL,
    slab_index     INTEGER NOT NULL,
    slab_id        TEXT,               -- slab digest (hex), for lookup
    version        INTEGER NOT NULL,
    encryption_key TEXT NOT NULL,      -- 32-byte slab key (hex)
    min_shards     INTEGER NOT NULL,   -- data shards needed to recover the slab
    byte_offset    INTEGER NOT NULL,   -- offset within the object
    byte_length    INTEGER NOT NULL,
    PRIMARY KEY (connection_id, object_id, slab_index),
    FOREIGN KEY (connection_id, object_id)
        REFERENCES objects(connection_id, object_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS object_sectors (
    connection_id INTEGER NOT NULL,
    object_id     TEXT NOT NULL,
    slab_index    INTEGER NOT NULL,
    sector_index  INTEGER NOT NULL,
    root          TEXT NOT NULL,       -- Merkle root (hex)
    host_key      TEXT NOT NULL,       -- host storing this sector
    PRIMARY KEY (connection_id, object_id, slab_index, sector_index),
    FOREIGN KEY (connection_id, object_id, slab_index)
        REFERENCES object_slabs(connection_id, object_id, slab_index) ON DELETE CASCADE
);

-- Back the id search: prefix range scans on slab id / sector root per connection.
CREATE INDEX IF NOT EXISTS idx_slabs_slab_id ON object_slabs (connection_id, slab_id);
CREATE INDEX IF NOT EXISTS idx_sectors_root ON object_sectors (connection_id, root);
