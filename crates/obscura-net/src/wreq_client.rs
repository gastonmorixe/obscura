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
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36";

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
        //   * Chrome147 is the freshest variant wreq-util 3.0.0-rc.11 ships (live
        //     Chrome stable is 149 as of 2026-05; wreq-util tracks one or two
        //     minor versions behind).
        //   * MacOS is intentional: most desktop visitors hit Bloomberg/PerimeterX
        //     from macOS or Windows. Linux Chrome is a much rarer fingerprint and
        //     gets scored harder by PX. Picking macOS keeps the UA, sec-ch-ua-
        //     platform, and (downstream) `navigator.platform` consistent for the
        //     common case. Note: wreq-util still reuses the v132 TLS+H2 build for
        //     every Chrome 132..=147 profile, so this isn't a TLS-fingerprint
        //     upgrade — only headers + UA. If PX starts blocking v132 ClientHello
        //     bytes wholesale we'll need a more recent wreq-util.
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

            // wreq-util's Chrome emulation injects sec-ch-ua, sec-ch-ua-mobile,
            // sec-ch-ua-platform, sec-fetch-dest, sec-fetch-mode, sec-fetch-site,
            // user-agent, accept, accept-encoding, accept-language, and priority
            // — but NOT `sec-fetch-user` or `upgrade-insecure-requests`, which
            // real Chrome sends on every top-level user-initiated navigation.
            // Their absence is one of the easier tells for PerimeterX-class
            // bot management (see Bloomberg 2026-05 block postmortem).
            //
            // Also override `accept` so its signed-exchange q-value matches what
            // live Chrome ships (0.7, not the 0.9 baked into wreq-util's macro).
            //
            // `hop == 0` is the initial navigation; subsequent hops are redirect
            // follow-ups where Chrome flips `sec-fetch-site` from `none` to
            // `same-origin`/`same-site`/`cross-site` and drops `sec-fetch-user`.
            // We can't tell the precise site relationship without extra plumbing,
            // so on redirects we only add `upgrade-insecure-requests` and let
            // wreq-util's defaults stand for the fetch metadata.
            if hop == 0 {
                req = req
                    .header(
                        "accept",
                        "text/html,application/xhtml+xml,application/xml;q=0.9,\
                         image/avif,image/webp,image/apng,*/*;q=0.8,\
                         application/signed-exchange;v=b3;q=0.7",
                    )
                    .header("sec-fetch-user", "?1")
                    .header("upgrade-insecure-requests", "1");
            } else {
                req = req.header("upgrade-insecure-requests", "1");
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
