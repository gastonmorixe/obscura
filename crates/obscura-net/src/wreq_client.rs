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
/// switched to a 2-brand GREASE format. Override on hop 0 so the wire
/// identity matches live Chrome 148 (Bloomberg/PerimeterX + WSJ/DataDome).
#[cfg(feature = "stealth")]
pub const STEALTH_SEC_CH_UA: &str = "\"Not/A)Brand\";v=\"99\", \"Chromium\";v=\"148\"";

// The wreq emulation platform must agree with navigator / sec-ch-ua-platform.
// Private branch intentionally uses macOS: Linux Chrome is a rarer fingerprint
// and gets scored harder by PerimeterX.
#[cfg(feature = "stealth")]
pub const STEALTH_NAVIGATOR_PLATFORM: &str = "MacIntel";
#[cfg(feature = "stealth")]
pub const STEALTH_UA_PLATFORM: &str = "macOS";
#[cfg(feature = "stealth")]
pub const STEALTH_UA_PLATFORM_VERSION: &str = "14.6.0";

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
        // Port of private gm/private stealth intent onto main's wreq-util API:
        // freshest Chrome profile + macOS platform, then hop-0 header overrides
        // in fetch() for Chrome 148 version identity (sec-ch-ua + UA).
        // accept-encoding is left to the emulation (emulation-compression feature
        // in Cargo.toml) so wire order stays Chrome-correct and bodies still
        // decompress. Do NOT add cache-control/pragma (DataDome fingerprint).
        let emulation_opts = wreq_util::Emulation::builder()
            .profile(wreq_util::Profile::Chrome145)
            .platform(wreq_util::Platform::MacOS)
            .build();

        let mut builder = wreq::Client::builder()
            .emulation(emulation_opts)
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

        if let Some(host) = current_url.host_str() {
            if crate::blocklist::is_blocked(host) {
                tracing::debug!("Blocked tracker: {}", current_url);
                return Ok(Response {
                    status: 0,
                    url: current_url,
                    headers: HashMap::new(),
                    body: Vec::new(),
                    redirected_from: Vec::new(),
                });
            }
        }

        let mut redirects = Vec::new();

        for hop in 0..20 {
            let mut req = self.client.get(current_url.as_str());

            // Version-identity overrides (private Chrome 148 work). Emulation
            // still owns accept-encoding / sec-fetch-* base set. Hop 0 adds
            // nav-only headers real Chrome sends on a top-level navigation.
            req = req
                .header("sec-ch-ua", STEALTH_SEC_CH_UA)
                .header("user-agent", STEALTH_USER_AGENT)
                .header("sec-ch-ua-platform", "\"macOS\"")
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

    /// One request with no redirect following, for scripted fetch()/XHR. Reads
    /// the cookie jar for the Cookie header and stores Set-Cookie back into it,
    /// so the caller only owns redirect hops and SSRF re-validation. Used in
    /// stealth mode so JS-level requests carry the same Chrome TLS fingerprint
    /// and client hints as the main navigation instead of the rustls ClientHello
    /// that op_fetch_url would otherwise send (which bot managers read as a
    /// non-browser script and reject, e.g. the AWS WAF challenge verify call).
    pub async fn send_single(
        &self,
        method: &str,
        url: &Url,
        headers: &HashMap<String, String>,
        body: &str,
    ) -> Result<Response, ObscuraNetError> {
        if let Some(host) = url.host_str() {
            if crate::blocklist::is_blocked(host) {
                tracing::debug!("Blocked tracker: {}", url);
                return Ok(Response {
                    status: 0,
                    url: url.clone(),
                    headers: HashMap::new(),
                    body: Vec::new(),
                    redirected_from: Vec::new(),
                });
            }
        }

        let req_method = method
            .parse::<wreq::Method>()
            .map_err(|e| ObscuraNetError::Network(format!("invalid method '{}': {}", method, e)))?;
        let mut req = self.client.request(req_method, url.as_str());

        let cookie_header = self.cookie_jar.get_cookie_header(url);
        if !cookie_header.is_empty() {
            req = req.header("cookie", &cookie_header);
        }
        for (k, v) in self.extra_headers.read().await.iter() {
            req = req.header(k.as_str(), v.as_str());
        }
        for (k, v) in headers.iter() {
            req = req.header(k.as_str(), v.as_str());
        }
        if !body.is_empty() {
            req = req.body(body.to_string());
        }

        self.in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let resp = req.send().await.map_err(|e| {
            self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            ObscuraNetError::Network(format!("{}: {}", url, e))
        })?;
        self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

        let status = resp.status();
        for val in resp.headers().get_all("set-cookie") {
            if let Ok(s) = val.to_str() {
                self.cookie_jar.set_cookie(s, url);
            }
        }
        let response_headers: HashMap<String, String> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let resp_body = resp
            .bytes()
            .await
            .map_err(|e| ObscuraNetError::Network(format!("Failed to read body: {}", e)))?
            .to_vec();

        Ok(Response {
            url: url.clone(),
            status: status.as_u16(),
            headers: response_headers,
            body: resp_body,
            redirected_from: Vec::new(),
        })
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
