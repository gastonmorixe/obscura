//! Persistent backing for the JS-facing `localStorage` Storage object.
//!
//! Real browsers scope `localStorage` per origin and persist it across
//! navigations and process restarts. obscura's V8 isolate is rebuilt on every
//! navigation (see `Page::init_js`), so the JS-side `localStorage` closure has
//! historically died with each page. This module provides a process-wide
//! Rust-side store that the bootstrap shim proxies through, keyed by the
//! origin of the page that's making the call.
//!
//! ## Design
//!
//! - One `HashMap<origin, HashMap<key, value>>` behind an `RwLock`.
//! - JSON file at `{storage_dir}/localstorage.json` mirroring the in-memory
//!   map. Single file for simplicity; this is small data.
//! - Atomic save via `tempfile::NamedTempFile::new_in(...).persist(target)`,
//!   matching the `CookieJar` approach.
//! - Eager load on creation, save on `BrowserContext::save_session`.
//!
//! ## What this is NOT
//!
//! - Not encrypted. Sites store auth tokens in here; the file is plaintext.
//!   Callers protect the directory.
//! - Not cross-process safe. Two obscura instances pointed at the same
//!   `--storage-dir` will race. Same constraint as the cookie jar.
//! - Not `sessionStorage`. Real browsers don't persist sessionStorage either;
//!   the bootstrap shim keeps that as the existing in-memory closure.

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

/// Per-origin key/value store mirroring the Web Storage API.
pub struct LocalStorageStore {
    inner: RwLock<HashMap<String, HashMap<String, String>>>,
}

impl LocalStorageStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Read one key from one origin's bucket. Returns `None` when the key (or
    /// the origin) is absent. Mirrors `Storage.getItem` returning `null`.
    pub fn get_item(&self, origin: &str, key: &str) -> Option<String> {
        let guard = self.inner.read().unwrap();
        guard
            .get(origin)
            .and_then(|bucket| bucket.get(key))
            .cloned()
    }

    /// Write one key to one origin's bucket. Bucket is created on demand.
    pub fn set_item(&self, origin: &str, key: &str, value: &str) {
        if origin.is_empty() {
            return;
        }
        let mut guard = self.inner.write().unwrap();
        guard
            .entry(origin.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
    }

    /// Remove one key from one origin's bucket. No-op when missing.
    pub fn remove_item(&self, origin: &str, key: &str) {
        let mut guard = self.inner.write().unwrap();
        if let Some(bucket) = guard.get_mut(origin) {
            bucket.remove(key);
        }
    }

    /// Drop every key for one origin. The origin entry is removed entirely
    /// so `length()` returns 0 after the call.
    pub fn clear_origin(&self, origin: &str) {
        let mut guard = self.inner.write().unwrap();
        guard.remove(origin);
    }

    /// Number of keys in one origin's bucket. 0 for unknown origins.
    pub fn length(&self, origin: &str) -> usize {
        let guard = self.inner.read().unwrap();
        guard.get(origin).map(|b| b.len()).unwrap_or(0)
    }

    /// Indexed key access. The order matches `HashMap` iteration order,
    /// which is unspecified but stable within one process for one bucket
    /// (no inserts in between). Real browsers also don't promise a specific
    /// order across reloads, so sites that depend on one are already broken.
    pub fn key_at(&self, origin: &str, index: usize) -> Option<String> {
        let guard = self.inner.read().unwrap();
        let bucket = guard.get(origin)?;
        bucket.keys().nth(index).cloned()
    }

    /// Wipe the entire store. Used by tests and `disposeBrowserContext`.
    pub fn clear_all(&self) {
        self.inner.write().unwrap().clear();
    }

    /// Snapshot of the entire store. Cloning is cheap for the typical
    /// hundred-bytes-per-origin case; if it ever isn't we'll switch to a
    /// streaming serialiser.
    pub fn snapshot(&self) -> HashMap<String, HashMap<String, String>> {
        self.inner.read().unwrap().clone()
    }

    /// Number of origins currently tracked. Distinct from `length(origin)`.
    pub fn origin_count(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    /// Serialise to a JSON file atomically. Creates parent dirs.
    ///
    /// The on-disk shape is `{origin: {key: value}}`, sorted for stable
    /// diffs (helpful when the file is committed by mistake or inspected by
    /// hand). `serde_json::to_string_pretty` already sorts string keys
    /// deterministically.
    pub fn save_to_file(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;

        let snapshot = self.inner.read().unwrap();
        // BTreeMap sort for stable output regardless of HashMap iteration order.
        let sorted: std::collections::BTreeMap<
            &String,
            std::collections::BTreeMap<&String, &String>,
        > = snapshot
            .iter()
            .map(|(o, kv)| (o, kv.iter().collect()))
            .collect();

        let json = serde_json::to_string_pretty(&sorted)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(json.as_bytes())?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }

    /// Load a JSON file into the store, merging with anything already there
    /// (keys in the file overwrite existing keys for the same origin).
    /// Missing file is fine and returns `Ok(0)` — the typical first-run case.
    /// Returns the number of origins loaded.
    pub fn load_from_file(&self, path: &Path) -> std::io::Result<usize> {
        if !path.exists() {
            return Ok(0);
        }
        let data = std::fs::read_to_string(path)?;
        let parsed: HashMap<String, HashMap<String, String>> = serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let n = parsed.len();
        let mut guard = self.inner.write().unwrap();
        for (origin, kv) in parsed {
            let bucket = guard.entry(origin).or_default();
            for (k, v) in kv {
                bucket.insert(k, v);
            }
        }
        Ok(n)
    }
}

impl Default for LocalStorageStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Derive the canonical origin string for a URL, matching the form that
/// `Origin.ascii_serialization()` produces (`scheme://host[:port]`).
///
/// Returns `None` for opaque origins (`about:blank`, `data:`, `file:` without
/// host, unparseable URLs). Callers should treat that as "don't touch the
/// store" — real browsers also do nothing for opaque-origin pages, except
/// they cap a tiny per-Document Storage object that ours conveniently isn't.
pub fn origin_of(url_str: &str) -> Option<String> {
    let url = url::Url::parse(url_str).ok()?;
    let origin = url.origin();
    if !origin.is_tuple() {
        return None;
    }
    Some(origin.ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_round_trip() {
        let s = LocalStorageStore::new();
        s.set_item("https://example.com", "k", "v");
        assert_eq!(s.get_item("https://example.com", "k"), Some("v".into()));
        assert_eq!(s.get_item("https://example.com", "missing"), None);
        assert_eq!(s.get_item("https://other.com", "k"), None);
    }

    #[test]
    fn remove_drops_only_target_key() {
        let s = LocalStorageStore::new();
        s.set_item("https://a.test", "k1", "v1");
        s.set_item("https://a.test", "k2", "v2");
        s.remove_item("https://a.test", "k1");
        assert_eq!(s.get_item("https://a.test", "k1"), None);
        assert_eq!(s.get_item("https://a.test", "k2"), Some("v2".into()));
    }

    #[test]
    fn clear_origin_drops_only_that_origin() {
        let s = LocalStorageStore::new();
        s.set_item("https://a.test", "k", "v");
        s.set_item("https://b.test", "k", "v");
        s.clear_origin("https://a.test");
        assert_eq!(s.get_item("https://a.test", "k"), None);
        assert_eq!(s.get_item("https://b.test", "k"), Some("v".into()));
        assert_eq!(s.length("https://a.test"), 0);
        assert_eq!(s.length("https://b.test"), 1);
    }

    #[test]
    fn length_and_key_at_match_storage_spec() {
        let s = LocalStorageStore::new();
        s.set_item("https://x.test", "a", "1");
        s.set_item("https://x.test", "b", "2");
        s.set_item("https://x.test", "c", "3");
        assert_eq!(s.length("https://x.test"), 3);
        // We don't promise an order; we promise key_at(i) for 0..length
        // returns each key exactly once.
        let mut seen: Vec<String> = (0..3)
            .filter_map(|i| s.key_at("https://x.test", i))
            .collect();
        seen.sort();
        assert_eq!(seen, vec!["a", "b", "c"]);
        assert_eq!(s.key_at("https://x.test", 99), None);
    }

    #[test]
    fn save_load_round_trip_preserves_per_origin_scoping() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("localstorage.json");

        let a = LocalStorageStore::new();
        a.set_item("https://gooseup.me", "theme", "dark");
        a.set_item("https://gooseup.me", "user", "gaston");
        a.set_item("https://twitter.com", "device_id", "abc123");
        a.save_to_file(&path).unwrap();
        assert!(path.exists());

        let b = LocalStorageStore::new();
        let n = b.load_from_file(&path).unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            b.get_item("https://gooseup.me", "theme"),
            Some("dark".into())
        );
        assert_eq!(
            b.get_item("https://gooseup.me", "user"),
            Some("gaston".into())
        );
        assert_eq!(
            b.get_item("https://twitter.com", "device_id"),
            Some("abc123".into())
        );
        // Cross-origin isolation survived the round trip.
        assert_eq!(b.get_item("https://twitter.com", "theme"), None);
    }

    #[test]
    fn load_nonexistent_file_returns_zero() {
        let s = LocalStorageStore::new();
        let n = s
            .load_from_file(Path::new("/nonexistent/path/localstorage.json"))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn save_creates_parent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b").join("ls.json");
        let s = LocalStorageStore::new();
        s.set_item("https://x.test", "k", "v");
        s.save_to_file(&nested).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn save_with_empty_store_is_valid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ls.json");
        let s = LocalStorageStore::new();
        s.save_to_file(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn empty_origin_is_a_noop_for_writes() {
        let s = LocalStorageStore::new();
        s.set_item("", "k", "v");
        assert_eq!(s.length(""), 0);
        assert_eq!(s.get_item("", "k"), None);
    }

    #[test]
    fn snapshot_clones_entire_store() {
        let s = LocalStorageStore::new();
        s.set_item("https://a.test", "k", "v");
        let snap = s.snapshot();
        assert_eq!(snap.get("https://a.test").unwrap().get("k").unwrap(), "v");
        // Mutating snapshot does not touch the store.
        let mut snap = snap;
        snap.clear();
        assert_eq!(s.get_item("https://a.test", "k"), Some("v".into()));
    }

    #[test]
    fn origin_of_canonicalises_default_port() {
        assert_eq!(
            origin_of("https://example.com/path/inside?q=1#frag").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            origin_of("http://example.com:8080/").as_deref(),
            Some("http://example.com:8080")
        );
        assert_eq!(
            origin_of("https://example.com:443/").as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn origin_of_returns_none_for_opaque() {
        // about:blank and data: URIs have opaque origins per HTML spec.
        assert_eq!(origin_of("about:blank"), None);
        assert_eq!(origin_of("data:text/html,<h1>hi</h1>"), None);
        assert_eq!(origin_of("not a url"), None);
    }

    #[test]
    fn merge_load_overwrites_keys_per_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ls.json");

        let a = LocalStorageStore::new();
        a.set_item("https://x.test", "k", "old");
        a.save_to_file(&path).unwrap();

        let b = LocalStorageStore::new();
        b.set_item("https://x.test", "k", "preexisting");
        b.set_item("https://x.test", "other", "kept");
        b.load_from_file(&path).unwrap();

        // File value wins for shared key, sibling key on same origin survives.
        assert_eq!(b.get_item("https://x.test", "k"), Some("old".into()));
        assert_eq!(b.get_item("https://x.test", "other"), Some("kept".into()));
    }
}
