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
        //     only the *version-identity* wire headers below to look like
        //     Chrome 148: sec-ch-ua brand list (148 dropped "Google Chrome")
        //     and the user-agent string. Everything else Chrome sends on a
        //     top-level navigation (accept-encoding, accept-language, the
        //     sec-fetch-* set, priority) now comes straight from the
        //     emulation's own header table so it lands in Chrome's native
        //     wire order. The only extra hop-0 additions are `accept`
        //     (q=0.7 signed-exchange, matching live 148 exactly),
        //     `upgrade-insecure-requests`, `priority`, and `sec-fetch-user`.
        //   * accept-encoding: the `emulation-compression` feature on
        //     wreq-util makes the Chrome147 profile advertise
        //     `gzip, deflate, br, zstd` in its native slot (right after
        //     `accept`). This is paired with wreq's gzip/brotli/deflate/zstd
        //     decode features (see obscura-net/Cargo.toml) so the body is
        //     still auto-decompressed. DataDome (WSJ) scores the *absence* of
        //     accept-encoding as a bot signal, so we must advertise it. We do
        //     NOT set it manually here — letting the emulation own it keeps
        //     the wire order Chrome-correct.
        //   * We deliberately do NOT send `cache-control: no-cache` /
        //     `pragma: no-cache`. Earlier revs added them as an "incognito"
        //     tell, but a live Chrome 148 top-level navigation (captured via
        //     CDP off this same machine, 2026-06) sends neither, and their
        //     presence is exactly the kind of header-set drift DataDome
        //     fingerprints. A normal address-bar navigation has no
        //     cache-control/pragma at all.
        //   * MacOS is intentional: most desktop visitors hit
        //     Bloomberg/PerimeterX from macOS or Windows. Linux Chrome is
        //     a much rarer fingerprint and gets scored harder by PX.
        //     Picking macOS keeps the UA, sec-ch-ua-platform, and
        //     (downstream) `navigator.platform` consistent for the common
        //     case.
        //   * Note: wreq-util reuses the v132 TLS+H2 build for every
        //     Chrome 132..=147 profile. That template is still modern
        //     (X25519MLKEM768, permuted extensions, PSK) and its JA4
        //     (t13d1514h2_8daaf6152771_9a55b862dad6) and HTTP/2 akamai
        //     digest match a live Chrome 148 byte-for-byte as of 2026-06, so
        //     the transport fingerprint is not the bottleneck — the request
        //     header set is. If a future detector starts blocking the v132
        //     ClientHello wholesale we'll need a fresher wreq-util release.
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

            // wreq-util's Chrome147 emulation already injects, in Chrome's
            // native wire order: sec-ch-ua, sec-ch-ua-mobile,
            // sec-ch-ua-platform, user-agent, the sec-fetch-* set, accept,
            // accept-encoding (via the `emulation-compression` feature —
            // gzip,deflate,br,zstd), accept-language, and priority. We only
            // override the two *version-identity* values it gets wrong for
            // 148, then add the handful of nav-only headers the emulation
            // doesn't carry. Everything we set with `.header()` that already
            // exists in the emulation table replaces the value in place
            // (wreq re-sorts to the emulation's OrigHeaderMap order before
            // sending), so the wire order stays Chrome-correct.
            //
            //   sec-ch-ua:  "Not/A)Brand";v="99", "Chromium";v="148"
            //               (emulation ships the 3-brand v147 shape; 148
            //                dropped the "Google Chrome" brand)
            //   user-agent: …Chrome/148.0.0.0…   (emulation: 147)
            //
            // hop-0-only additions (a fresh top-level navigation):
            //   accept:                    …signed-exchange;v=b3;q=0.7
            //                              (live 148 uses q=0.7, not q=0.9)
            //   upgrade-insecure-requests: 1
            //   priority:                  u=0, i
            //   sec-fetch-user:            ?1
            //
            // We do NOT touch accept-encoding (owned by the emulation, kept
            // in lockstep with wreq's decode features) and we deliberately
            // send NO cache-control/pragma — a real Chrome 148 address-bar
            // navigation (CDP capture off this machine, 2026-06) sends
            // neither, and DataDome scores that header-set drift.
            //
            // On redirect follow-ups Chrome flips sec-fetch-site away from
            // `none` and drops sec-fetch-user; we can't infer the exact
            // site relationship without extra plumbing, so we only re-assert
            // the version identity on later hops and let the emulation's
            // fetch metadata stand.
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
