# revelio

A GUI tool for navigating app data of [Sia Storage](https://sia.storage) apps.

It connects to an indexer by using the same app id as the app for which you want
to inspect the uploaded data. It then syncs the apps' objects into a local
SQLite database for inspection.

## Features

- Check what objects an app pins on Sia Storage
- Download and upload objects
- Read and edit object metadata
- Search and filter objects by id, slab id, sector id, metadata and more
- Visualize the distribution of uploaded data
- Check details of usable hosts

## Usage

Run it with the following command (reqires a recent Rust toolchain):
```sh
cargo run --release
```

Logs go to **stdout**; control verbosity with `RUST_LOG`:

```sh
RUST_LOG=info,revelio=debug,sia_storage=info cargo run --release
```

## Persistence

State lives in a single SQLite database in the platform's data directory:

- **macOS:** `~/Library/Application Support/tech.ChrisSchinnerl.revelio/revelio.db`
- **Linux:** `$XDG_DATA_HOME/revelio/revelio.db` (usually `~/.local/share/revelio`)
- **Windows:** `%APPDATA%\ChrisSchinnerl\revelio\data\revelio.db`

The schema is in [`src/init.sql`](src/init.sql), applied on open. There are no
migrations — on a schema change, `revelio reset` and re-sync.

To wipe all local data (database, connections, keys, synced objects):

```sh
revelio reset
```

This only removes revelio's local data directory; indexer accounts are untouched.

## License

MIT
