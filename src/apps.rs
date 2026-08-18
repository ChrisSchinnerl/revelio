//! Registry of known apps to impersonate. Only the app id matters for key
//! derivation; the rest of the metadata is generic.

use anyhow::{Context, Result};
use sia_storage::{AppID, AppMetadata};

/// A known app the explorer can impersonate, identified purely by its app id.
pub struct KnownApp {
    pub name: &'static str,
    pub app_id: &'static str,
}

/// The apps we know the ids of; the UI adds a "Custom…" entry on top.
pub const KNOWN_APPS: &[KnownApp] = &[
    KnownApp {
        // s3d derives its id as blake2b256("s3d").
        name: "s3d",
        app_id: "4264b229d9473e9601684b2a84058c2434d0bb79a21d48738b9ea9b66756763e",
    },
    KnownApp {
        name: "fermata",
        app_id: "f964e8b62fd9b25b74c1acfb87e6e2560426c06bebe41873fb055b22ace0d4ee",
    },
    KnownApp {
        // The "Sia Storage" app (sia-storage-app); same id across mobile/desktop.
        name: "sia mobile",
        app_id: "ac38d91cfb250d50820a0c658628662b8c2dcfc6a5f3fe4d5755eb0a7b67eeac",
    },
];

/// Returns the display name for a known app id (hex), if recognized.
pub fn app_name_for(app_id_hex: &str) -> Option<&'static str> {
    let id = app_id_hex.trim();
    KNOWN_APPS
        .iter()
        .find(|a| a.app_id.eq_ignore_ascii_case(id))
        .map(|a| a.name)
}

/// Parses a 64-char hex app id into an [`AppID`].
pub fn parse_app_id(s: &str) -> Result<AppID> {
    let s = s.trim();
    s.parse()
        .with_context(|| format!("invalid app id (expected 64 hex chars): {s:?}"))
}

/// Generic metadata for every connection. Only `id` varies; the display fields
/// just affect how the approval screen looks in indexd.
pub fn app_metadata(id: AppID) -> AppMetadata {
    AppMetadata {
        id,
        name: "revelio",
        description: "Sia object explorer / debug tool",
        service_url: "https://sia.storage",
        logo_url: None,
        callback_url: None,
    }
}
