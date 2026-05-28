use std::path::PathBuf;
use std::sync::Arc;

use obscura_ext::ExtensionRuntime;
use obscura_net::{CookieJar, LocalStorageStore, ObscuraHttpClient, RobotsCache};

pub struct BrowserContext {
    pub id: String,
    pub cookie_jar: Arc<CookieJar>,
    /// Process-wide localStorage backing store, scoped per origin.
    /// Survives navigation (the JS runtime gets re-bound to the same
    /// `Arc` on every page) and persists to `{storage_dir}/localstorage.json`
    /// when `storage_dir` is set. See `obscura_net::localstorage` for the
    /// data model and `obscura_js::ops::op_localstorage_*` for the ops
    /// that bootstrap.js routes `Storage.getItem` / `.setItem` through.
    pub localstorage_store: Arc<LocalStorageStore>,
    pub http_client: Arc<ObscuraHttpClient>,
    pub user_agent: String,
    pub proxy_url: Option<String>,
    pub robots_cache: Arc<RobotsCache>,
    pub obey_robots: bool,
    pub stealth: bool,
    /// When true, CDP-driven navigation to file:// URLs is permitted.
    /// Default is false: a remote CDP client cannot point the browser
    /// at /etc/shadow even if Obscura is running as a privileged user.
    /// Flip on via `obscura serve --allow-file-access` for legitimate
    /// local-HTML testing workflows. The CLI's own `obscura fetch
    /// file://...` path is unaffected because it does not go through
    /// the CDP server.
    pub allow_file_access: bool,
    pub storage_dir: Option<PathBuf>,
    /// Optional loaded WebExtension. When set, every `Page` created from
    /// this context will inject the extension's background scripts +
    /// chrome.* shim as a preload script before page JS runs. See the
    /// `obscura-ext` crate for the host shim that ferries
    /// `runtime.sendMessage`/`tabs.executeScript`/`storage.local` between
    /// the extension and the page realm.
    pub extension: Option<Arc<ExtensionRuntime>>,
}

impl BrowserContext {
    pub fn new(id: String) -> Self {
        Self::_new_inner(id, None, false, None, None)
    }

    /// Create a BrowserContext with an optional storage directory.
    /// When `storage_dir` is set, cookies are automatically loaded from
    /// `{storage_dir}/cookies.json` on creation.
    pub fn with_storage(
        id: String,
        storage_dir: Option<PathBuf>,
    ) -> Self {
        Self::_new_inner(id, None, false, None, storage_dir)
    }

    /// Attach a loaded WebExtension. Returns `self` for chaining at the
    /// call site (`BrowserContext::new(...).with_extension(ext)`).
    pub fn with_extension(mut self, ext: Arc<ExtensionRuntime>) -> Self {
        self.extension = Some(ext);
        self
    }

    /// Create a BrowserContext with full options including storage_dir.
    pub fn with_storage_full(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<PathBuf>,
    ) -> Self {
        Self::_new_inner(id, proxy_url, stealth, user_agent, storage_dir)
    }

    fn _new_inner(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<PathBuf>,
    ) -> Self {
        let cookie_jar = Arc::new(CookieJar::new());
        let localstorage_store = Arc::new(LocalStorageStore::new());

        // Restore both halves of the session from disk if storage_dir is set.
        // Eager-load is fine: cookies + localStorage for the typical site are
        // both small (~kB), and any miss is a cheap missing-file return.
        if let Some(ref dir) = storage_dir {
            let cookie_path = dir.join("cookies.json");
            if cookie_path.exists() {
                match cookie_jar.load_from_file(&cookie_path) {
                    Ok(n) if n > 0 => {
                        tracing::info!("Loaded {} cookies from {}", n, cookie_path.display());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("Failed to load cookies from {}: {}", cookie_path.display(), e);
                    }
                }
            }
            let ls_path = dir.join("localstorage.json");
            if ls_path.exists() {
                match localstorage_store.load_from_file(&ls_path) {
                    Ok(n) if n > 0 => {
                        tracing::info!(
                            "Loaded localStorage for {} origin(s) from {}",
                            n,
                            ls_path.display()
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            "Failed to load localStorage from {}: {}",
                            ls_path.display(),
                            e
                        );
                    }
                }
            }
        }

        let mut client = ObscuraHttpClient::with_options(
            cookie_jar.clone(),
            proxy_url.as_deref(),
        );
        if stealth {
            client.block_trackers = true;
        }
        let resolved_ua = user_agent.unwrap_or_else(|| {
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36".to_string()
        });
        // Sync the http client's UA at construction so navigation requests pick it
        // up before any async setup runs. The lock has no other holders here, so
        // try_write always succeeds; we fall back silently if it ever fails.
        if let Ok(mut guard) = client.user_agent.try_write() {
            *guard = resolved_ua.clone();
        }
        let http_client = Arc::new(client);
        BrowserContext {
            id,
            cookie_jar,
            localstorage_store,
            http_client,
            user_agent: resolved_ua,
            proxy_url,
            robots_cache: Arc::new(RobotsCache::new()),
            obey_robots: false,
            stealth,
            allow_file_access: false,
            storage_dir,
            extension: None,
        }
    }

    pub fn with_options(id: String, proxy_url: Option<String>, stealth: bool) -> Self {
        Self::with_full_options(id, proxy_url, stealth, None)
    }

    pub fn with_full_options(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
    ) -> Self {
        Self::_new_inner(id, proxy_url, stealth, user_agent, None)
    }

    pub fn with_proxy(id: String, proxy_url: Option<String>) -> Self {
        Self::with_options(id, proxy_url, false)
    }

    /// Persist the full session (cookies + localStorage) to disk if
    /// `storage_dir` is configured. Called during graceful shutdown
    /// (CLI exit, CDP server Ctrl-C, MCP `browser_close`).
    ///
    /// Both writes are independent: a failure to write one does not
    /// prevent the other. Errors are logged at `warn!` since callers
    /// generally cannot do anything useful with them at the point of
    /// invocation (we're shutting down).
    pub fn save_session(&self) {
        let Some(ref dir) = self.storage_dir else {
            return;
        };
        let _ = std::fs::create_dir_all(dir);

        let cookie_path = dir.join("cookies.json");
        if let Err(e) = self.cookie_jar.save_to_file(&cookie_path) {
            tracing::warn!(
                "Failed to save cookies to {}: {}",
                cookie_path.display(),
                e
            );
        } else {
            tracing::info!("Saved cookies to {}", cookie_path.display());
        }

        let ls_path = dir.join("localstorage.json");
        if let Err(e) = self.localstorage_store.save_to_file(&ls_path) {
            tracing::warn!(
                "Failed to save localStorage to {}: {}",
                ls_path.display(),
                e
            );
        } else {
            tracing::info!(
                "Saved localStorage ({} origin(s)) to {}",
                self.localstorage_store.origin_count(),
                ls_path.display()
            );
        }
    }

    /// Backwards-compatible alias. New callers should use `save_session`.
    #[deprecated(note = "use save_session, which also persists localStorage")]
    pub fn save_cookies(&self) {
        self.save_session();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn with_full_options_propagates_user_agent_to_http_client() {
        let ctx = BrowserContext::with_full_options(
            "test".to_string(),
            None,
            false,
            Some("Custom-UA/1.0".to_string()),
        );
        assert_eq!(ctx.user_agent, "Custom-UA/1.0");
        let client_ua = ctx.http_client.user_agent.read().await.clone();
        assert_eq!(client_ua, "Custom-UA/1.0");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn with_full_options_falls_back_to_chrome_default() {
        let ctx = BrowserContext::with_full_options(
            "test".to_string(),
            None,
            false,
            None,
        );
        assert!(ctx.user_agent.contains("Chrome"));
        let client_ua = ctx.http_client.user_agent.read().await.clone();
        assert!(client_ua.contains("Chrome"));
        assert_eq!(ctx.user_agent, client_ua);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn with_options_keeps_default_user_agent() {
        let ctx = BrowserContext::with_options("test".to_string(), None, false);
        assert!(ctx.user_agent.contains("Chrome"));
    }
}
