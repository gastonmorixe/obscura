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
    pub platform: String,
    pub ua_platform: String,
    pub ua_platform_version: String,
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
    /// When true, the http client allows fetching localhost / RFC1918 /
    /// link-local addresses. Set via `--allow-private-network` (issue #33).
    /// Independent of `allow_file_access` because they cover different threat
    /// models: file:// is a local file-system read, while private-network is
    /// the broader SSRF gate from issue #4.
    pub allow_private_network: bool,
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
        Self::_new_inner(id, None, false, None, None, false)
    }

    /// Create a BrowserContext with an optional storage directory.
    /// When `storage_dir` is set, cookies are automatically loaded from
    /// `{storage_dir}/cookies.json` on creation.
    pub fn with_storage(
        id: String,
        storage_dir: Option<PathBuf>,
    ) -> Self {
        Self::_new_inner(id, None, false, None, storage_dir, false)
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
        Self::_new_inner(id, proxy_url, stealth, user_agent, storage_dir, false)
    }

    /// Variant that also accepts the `allow_private_network` opt-in. All
    /// pre-existing constructors default it to `false`; callers that want the
    /// CLI's `--allow-private-network` (issue #33) behaviour go through here.
    pub fn with_storage_and_network(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<PathBuf>,
        allow_private_network: bool,
    ) -> Self {
        Self::_new_inner(id, proxy_url, stealth, user_agent, storage_dir, allow_private_network)
    }

    fn _new_inner(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<PathBuf>,
        allow_private_network: bool,
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

        let mut client = ObscuraHttpClient::with_full_options(
            cookie_jar.clone(),
            proxy_url.as_deref(),
            allow_private_network,
        );
        if stealth {
            client.block_trackers = true;
        }
        let profile = crate::profiles::select_profile();
        // Prefer an explicit UA; otherwise use the selected profile. When
        // stealth is on and no UA was given, prefer the private Chrome 148
        // macOS identity that was tuned against Bloomberg/PerimeterX and
        // WSJ/DataDome rather than the first rotated profile entry.
        let stealth_default = stealth && user_agent.is_none();
        let resolved_ua = user_agent.unwrap_or_else(|| {
            if stealth {
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36".to_string()
            } else {
                profile.user_agent.to_string()
            }
        });
        let (platform, ua_platform, ua_platform_version) = if stealth_default {
            (
                "MacIntel".to_string(),
                "macOS".to_string(),
                "14.6.0".to_string(),
            )
        } else {
            (
                profile.platform.to_string(),
                profile.ua_platform.to_string(),
                profile.ua_platform_version.to_string(),
            )
        };
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
            platform,
            ua_platform,
            ua_platform_version,
            proxy_url,
            robots_cache: Arc::new(RobotsCache::new()),
            obey_robots: false,
            stealth,
            allow_file_access: false,
            storage_dir,
            allow_private_network,
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
        Self::_new_inner(id, proxy_url, stealth, user_agent, None, false)
    }

    pub fn with_proxy(id: String, proxy_url: Option<String>) -> Self {
        Self::with_options(id, proxy_url, false)
    }

    /// Create a context with the same browser configuration but independent
    /// mutable network state. Persistent copies start with the template's
    /// current cookies + localStorage; incognito copies start empty and never
    /// write to the template's storage directory.
    pub fn isolated_copy(&self, id: String, persistent: bool) -> Self {
        let cookie_jar = Arc::new(CookieJar::new());
        let localstorage_store = if persistent {
            // Share the same store so persisted origins remain visible; the
            // template's save_session still owns the on-disk write.
            self.localstorage_store.clone()
        } else {
            Arc::new(LocalStorageStore::new())
        };
        if persistent {
            cookie_jar.set_cookies_from_cdp(self.cookie_jar.get_all_cookies());
        }

        let mut client = ObscuraHttpClient::with_full_options(
            cookie_jar.clone(),
            self.proxy_url.as_deref(),
            self.allow_private_network,
        );
        if self.stealth {
            client.block_trackers = true;
        }
        if let Ok(mut guard) = client.user_agent.try_write() {
            *guard = self.user_agent.clone();
        }

        BrowserContext {
            id,
            cookie_jar,
            localstorage_store,
            http_client: Arc::new(client),
            user_agent: self.user_agent.clone(),
            platform: self.platform.clone(),
            ua_platform: self.ua_platform.clone(),
            ua_platform_version: self.ua_platform_version.clone(),
            proxy_url: self.proxy_url.clone(),
            robots_cache: Arc::new(RobotsCache::new()),
            obey_robots: self.obey_robots,
            stealth: self.stealth,
            allow_file_access: self.allow_file_access,
            storage_dir: persistent.then(|| self.storage_dir.clone()).flatten(),
            allow_private_network: self.allow_private_network,
            extension: self.extension.clone(),
        }
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

    #[tokio::test(flavor = "current_thread")]
    async fn isolated_copy_does_not_share_mutable_network_state() {
        let source = BrowserContext::with_full_options(
            "source".to_string(),
            None,
            false,
            Some("Template-UA/1.0".to_string()),
        );
        source.cookie_jar.set_cookie("sid=source", &url::Url::parse("https://example.com").unwrap());

        let persistent = source.isolated_copy("persistent".to_string(), true);
        let incognito = source.isolated_copy("incognito".to_string(), false);

        assert_eq!(persistent.cookie_jar.get_all_cookies().len(), 1);
        assert!(incognito.cookie_jar.get_all_cookies().is_empty());
        persistent.cookie_jar.clear();
        persistent.http_client.set_user_agent("Changed-UA/2.0").await;

        assert_eq!(source.cookie_jar.get_all_cookies().len(), 1);
        assert_eq!(source.http_client.user_agent.read().await.as_str(), "Template-UA/1.0");
    }
}
