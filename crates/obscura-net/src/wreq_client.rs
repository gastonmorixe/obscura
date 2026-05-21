#[cfg(feature = "stealth")]
use std::collections::HashMap;
#[cfg(feature = "stealth")]
use std::error::Error;
#[cfg(feature = "stealth")]
use std::sync::Arc;
#[cfg(feature = "stealth")]
use std::time::Duration;

#[cfg(feature = "stealth")]
use tokio::sync::RwLock;
#[cfg(feature = "stealth")]
use url::Url;

#[cfg(feature = "stealth")]
use crate::cookies::CookieJar;
#[cfg(feature = "stealth")]
use crate::client::{Response, ObscuraNetError};

#[cfg(feature = "stealth")]
pub const STEALTH_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";

/// Chrome 148's `sec-ch-ua` brand list. Chrome 148 dropped the
/// `"Google Chrome";v="…"` brand that earlier versions emitted and
/// switched to a 2-brand GREASE format. wreq-util 3.0.0-rc.11's
/// Chrome147 macro still emits the old 3-brand shape, so we override on
/// every initial hop (and the non-stealth path mirrors this constant
/// in `client.rs`).
///
/// Source: live Chrome 148.0.0.0 macOS HAR captured 2026-05-21 (see
/// `tmp/2-www.bloomberg.com.har`, top-level GET on
/// www.bloomberg.com).
#[cfg(feature = "stealth")]
pub const STEALTH_SEC_CH_UA: &str = "\"Not/A)Brand\";v=\"99\", \"Chromium\";v=\"148\"";

#[cfg(feature = "stealth")]
pub struct StealthHttpClient {
    client: wreq::Client,
    pub cookie_jar: Arc<CookieJar>,
    pub extra_headers: RwLock<HashMap<String, String>>,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
}

#[cfg(feature = "stealth")]
impl StealthHttpClient {
    pub fn new(cookie_jar: Arc<CookieJar>) -> Self {
        Self::with_proxy(cookie_jar, None)
    }

    pub fn with_proxy(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Self {
        // Issue #184: `set_default_paths()` reads OpenSSL's compile-time CA
        // paths, which only resolve on Linux. On Windows the store ends up
        // empty and every TLS handshake fails with CERTIFICATE_VERIFY_FAILED.
        // `CertStore::default()` uses wreq's bundled Mozilla roots
        // (`webpki-root-certs`), which works the same on every platform.
        let cert_store = wreq::tls::CertStore::default();

        // Emulation profile:
        //   * Chrome147 is the freshest variant wreq-util 3.0.0-rc.11 ships
        //     (live Chrome stable is 148+ as of 2026-05). We keep the TLS+H2
        //     fingerprint that wreq-util computes for Chrome147 and override
        //     the *wire headers* below to look like Chrome 148: that's what
        //     PerimeterX/HUMAN actually scores on for the top-level
        //     navigation. The header overrides are: sec-ch-ua brand list
        //     (148 dropped "Google Chrome"), user-agent string, priority
        //     (Chrome 124+ ships this on every document GET), accept-
        //     encoding (Chrome 121+ ships zstd), cache-control + pragma
        //     (incognito-mode signal), plus the previous pass's accept,
        //     sec-fetch-user and upgrade-insecure-requests.
        //   * MacOS is intentional: most desktop visitors hit
        //     Bloomberg/PerimeterX from macOS or Windows. Linux Chrome is
        //     a much rarer fingerprint and gets scored harder by PX.
        //     Picking macOS keeps the UA, sec-ch-ua-platform, and
        //     (downstream) `navigator.platform` consistent for the common
        //     case.
        //   * Note: wreq-util still reuses the v132 TLS+H2 build for every
        //     Chrome 132..=147 profile, so the JA3/JA4 fingerprint here is
        //     v132's. If PX starts blocking v132 ClientHello bytes
        //     wholesale we'll need a fresher wreq-util release or to pin a
        //     different Emulation profile per target.
        let emulation_opts = wreq_util::EmulationOption::builder()
            .emulation(wreq_util::Emulation::Chrome147)
            .emulation_os(wreq_util::EmulationOS::MacOS)
            .build();

        let mut builder = wreq::Client::builder()
            .emulation(emulation_opts)
            .cert_store(cert_store)
            .timeout(Duration::from_secs(30))
            .redirect(wreq::redirect::Policy::none());

        if let Some(proxy) = proxy_url {
            if let Ok(p) = wreq::Proxy::all(proxy) {
                builder = builder.proxy(p);
            }
        }

        let client = builder.build().expect("failed to build wreq stealth client");

        StealthHttpClient {
            client,
            cookie_jar,
            extra_headers: RwLock::new(HashMap::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, ObscuraNetError> {
        let mut current_url = url.clone();
        let mut redirects = Vec::new();

        for hop in 0..20 {
            let mut req = self.client.get(current_url.as_str());

            // wreq-util's Chrome147 emulation injects sec-ch-ua,
            // sec-ch-ua-mobile, sec-ch-ua-platform, sec-fetch-dest,
            // sec-fetch-mode, sec-fetch-site, user-agent, accept,
            // accept-encoding, and accept-language — but the values it
            // picks are stale relative to live Chrome 148 and several
            // headers Chrome sends on every top-level navigation are
            // absent. Captured 200 OK from real Chrome 148.0.0.0 macOS
            // hitting Bloomberg (tmp/2-www.bloomberg.com.har) showed:
            //
            //   accept:            …;v=b3;q=0.7         (wreq-util: q=0.9)
            //   cache-control:     no-cache             (wreq-util: absent)
            //   pragma:            no-cache             (wreq-util: absent)
            //   priority:          u=0, i               (wreq-util: absent)
            //   sec-ch-ua:         "Not/A)Brand";v="99", "Chromium";v="148"
            //                      (wreq-util: 3-brand format with "Google
            //                       Chrome";v="147" — dropped in 148)
            //   sec-fetch-user:    ?1                   (wreq-util: absent)
            //   upgrade-insecure-requests: 1            (wreq-util: absent)
            //   user-agent:        …Chrome/148.0.0.0…   (wreq-util: 147)
            //
            // Each absence or version mismatch is a PerimeterX/HUMAN
            // scoring signal. We override the full set on the initial
            // hop. On redirect follow-ups Chrome flips `sec-fetch-site`
            // from `none` to `same-origin`/`same-site`/`cross-site`,
            // drops `sec-fetch-user`, and keeps `priority` /
            // `upgrade-insecure-requests` / `cache-control` / `pragma`
            // (when the user reloaded with no-cache). We can't tell the
            // precise site relationship without extra plumbing, so on
            // redirects we only re-assert the encoding + version
            // identity overrides and let wreq-util's fetch metadata
            // stand.
            //
            // NB: real Chrome 148 also ships `accept-encoding: gzip,
            // deflate, br, zstd`. We do NOT override `accept-encoding`
            // here. Setting it manually disables wreq's response-body
            // auto-decompression for any encoding the server picks,
            // turning what looks like a 200 OK into a stream of
            // un-decompressed bytes the dump pipeline can't parse. The
            // emulation's `gzip,deflate,br` (no zstd) advertisement is
            // therefore the remaining drift versus live Chrome 148.
            // Bloomberg's PerimeterX accepts it as of 2026-05; if a
            // future PX rev starts scoring zstd presence, the right
            // move is a fresher wreq-util that bakes zstd into its
            // Chrome emulation accept-encoding (still server-side
            // advertised, still auto-decompressed) rather than another
            // header override here.
            req = req
                .header("sec-ch-ua", STEALTH_SEC_CH_UA)
                .header("user-agent", STEALTH_USER_AGENT)
                .header("upgrade-insecure-requests", "1");
            if hop == 0 {
                req = req
                    .header(
                        "accept",
                        "text/html,application/xhtml+xml,application/xml;q=0.9,\
                         image/avif,image/webp,image/apng,*/*;q=0.8,\
                         application/signed-exchange;v=b3;q=0.7",
                    )
                    .header("cache-control", "no-cache")
                    .header("pragma", "no-cache")
                    .header("priority", "u=0, i")
                    .header("sec-fetch-user", "?1");
            }

            let cookie_header = self.cookie_jar.get_cookie_header(&current_url);
            if !cookie_header.is_empty() {
                req = req.header("Cookie", &cookie_header);
            }

            for (k, v) in self.extra_headers.read().await.iter() {
                req = req.header(k.as_str(), v.as_str());
            }

            self.in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let resp = req.send().await.map_err(|e| {
                self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                ObscuraNetError::Network(format!("{}: {} (source: {:?})", current_url, e, e.source()))
            })?;
            self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

            let status = resp.status();

            for val in resp.headers().get_all("set-cookie") {
                if let Ok(s) = val.to_str() {
                    self.cookie_jar.set_cookie(s, &current_url);
                }
            }

            let response_headers: HashMap<String, String> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
                .collect();

            if status.is_redirection() {
                if let Some(location) = resp.headers().get("location") {
                    let location_str = location.to_str().map_err(|_| {
                        ObscuraNetError::Network("Invalid redirect Location".into())
                    })?;
                    let next_url = current_url.join(location_str).map_err(|e| {
                        ObscuraNetError::Network(format!("Invalid redirect URL: {}", e))
                    })?;
                    redirects.push(current_url.clone());
                    current_url = next_url;
                    continue;
                }
            }

            let body = resp.bytes().await.map_err(|e| {
                ObscuraNetError::Network(format!("Failed to read body: {}", e))
            })?.to_vec();

            return Ok(Response {
                url: current_url,
                status: status.as_u16(),
                headers: response_headers,
                body,
                redirected_from: redirects,
            });
        }

        Err(ObscuraNetError::TooManyRedirects(url.to_string()))
    }

    pub async fn set_extra_headers(&self, headers: HashMap<String, String>) {
        *self.extra_headers.write().await = headers;
    }

    pub fn active_requests(&self) -> u32 {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn is_network_idle(&self) -> bool {
        self.active_requests() == 0
    }
}
