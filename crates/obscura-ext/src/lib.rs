//! Browser-extension host for Obscura.
//!
//! Loads an unpacked or packed WebExtension bundle (MV2 or MV3), parses
//! `manifest.json`, exposes the bundle's files for injection, and tracks
//! per-extension state (storage, registered webRequest/runtime
//! listeners, content-script invocation history) so it can be queried
//! by the rest of Obscura on each navigation.
//!
//! This crate is intentionally small: it does NOT execute JavaScript.
//! The JS runtime that hosts the extension's background and content
//! scripts lives in `obscura-js`. This crate is the *book-keeping*
//! layer that turns a manifest into "for URL X, run files Y in order,
//! apply webRequest listeners Z, expose storage W".
//!
//! Manifest V2 (Firefox-style, persistent background page) is the
//! reference target; MV3 bundles load but the service-worker lifecycle
//! is not emulated — the background runs as a persistent script.
//! See `docs/extensions.md` for the architectural deep dive.

pub mod bundle;
pub mod host_match;
pub mod manifest;
pub mod runtime;
pub mod state;

pub use bundle::{Bundle, BundleError};
pub use manifest::{ExtensionManifest, ManifestError};
pub use runtime::ExtensionRuntime;
pub use state::{ExtensionState, StorageArea};
