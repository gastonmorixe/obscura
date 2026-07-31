use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use obscura_browser::lifecycle::WaitUntil;
use obscura_browser::{BrowserContext, Page};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

const PROTOCOL_VERSION: u64 = 1;
const MAX_REQUEST_LINE: usize = 1024 * 1024;
const MAX_REQUEST_ID: usize = 128;
const MAX_URL: usize = 4096;
const MAX_SELECTOR: usize = 64 * 1024;
const MAX_EVAL: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SESSIONS: usize = 8;
const DEFAULT_IDLE_SECS: u64 = 300;
const MAX_TIMEOUT_MS: u64 = 120_000;

#[derive(Clone)]
struct WorkerConfig {
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
    allow_private_network: bool,
    idle_timeout: Duration,
    extension: Option<Arc<obscura_ext::ExtensionRuntime>>,
}

impl WorkerConfig {
    fn from_env() -> Self {
        let proxy = env_string("OBSCURA_PROXY");
        let user_agent = env_string("OBSCURA_USER_AGENT");
        let stealth = env_bool("OBSCURA_STEALTH");
        let allow_private_network = env_bool("OBSCURA_ALLOW_PRIVATE_NETWORK");
        let idle_secs = std::env::var("OBSCURA_FETCH_WORKER_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_IDLE_SECS);
        let extension = env_string("OBSCURA_FETCH_WORKER_EXTENSION")
            .and_then(|path| match obscura_ext::Bundle::load(&path) {
                Ok(bundle) => Some(Arc::new(obscura_ext::ExtensionRuntime::new(Arc::new(bundle)))),
                Err(error) => {
                    eprintln!("fetch worker extension load failed for {path}: {error}");
                    None
                }
            });
        Self {
            proxy,
            stealth,
            user_agent,
            allow_private_network,
            idle_timeout: Duration::from_secs(idle_secs),
            extension,
        }
    }
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_bool(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

#[derive(Debug, Deserialize)]
struct FetchParams {
    url: String,
    format: String,
    #[serde(default = "default_wait_until")]
    wait_until: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    settle_ms: u64,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    eval: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    storage_dir: Option<PathBuf>,
}

fn default_wait_until() -> String {
    "domcontentloaded".to_string()
}

fn default_timeout_ms() -> u64 {
    30_000
}

#[derive(Debug, Deserialize)]
struct CloseParams {
    session: String,
}

struct Session {
    context: Arc<BrowserContext>,
    page: Page,
    last_used: u64,
}

struct Worker {
    config: WorkerConfig,
    sessions: HashMap<String, Session>,
    active_session: Option<String>,
    sequence: u64,
    started: Instant,
    completed: u64,
    failed: u64,
    hello_done: bool,
}

impl Worker {
    fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            sessions: HashMap::new(),
            active_session: None,
            sequence: 0,
            started: Instant::now(),
            completed: 0,
            failed: 0,
            hello_done: false,
        }
    }

    async fn dispatch(&mut self, line: &[u8]) -> (Response, bool) {
        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(error) => return (Response::error("", "invalid_request", format!("invalid JSON: {error}")), false),
        };

        let id = match value.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() && id.len() <= MAX_REQUEST_ID => id.to_string(),
            _ => return (Response::error("", "invalid_request", "id must be a non-empty string of at most 128 bytes"), false),
        };
        let version = match value.get("v").and_then(Value::as_u64) {
            Some(version) => version,
            None => return (Response::error(&id, "invalid_request", "v must be an integer"), false),
        };
        if version != PROTOCOL_VERSION {
            return (Response::error(&id, "unsupported_version", format!("unsupported protocol version {version}")), false);
        }
        let op = match value.get("op").and_then(Value::as_str) {
            Some(op) => op,
            None => return (Response::error(&id, "invalid_request", "op must be a string"), false),
        };

        let (response, shutdown) = match op {
            "hello" => {
                self.hello_done = true;
                (Response::success(&id, json!({
                    "protocol": PROTOCOL_VERSION,
                    "pid": std::process::id(),
                    "operations": ["hello", "fetch", "status", "close_session", "shutdown"],
                    "supported_formats": ["html", "text", "links", "markdown", "accessibility"],
                    "max_response_bytes": MAX_RESPONSE_BYTES,
                    "max_sessions": MAX_SESSIONS
                })), false)
            }
            "fetch" if !self.hello_done => (
                Response::error(&id, "invalid_request", "hello must succeed before fetch"),
                false,
            ),
            "fetch" => (self.fetch(&id, value.get("params").cloned()).await, false),
            "status" => (Response::success(&id, self.status()), false),
            "close_session" => (self.close_session(&id, value.get("params").cloned()), false),
            "shutdown" => {
                self.cleanup();
                (Response::success(&id, json!({"shutdown": true})), true)
            }
            _ => (Response::error(&id, "unsupported_op", format!("unsupported operation '{op}'")), false),
        };

        if response.ok {
            self.completed += 1;
        } else {
            self.failed += 1;
        }
        (response, shutdown)
    }

    fn status(&self) -> Value {
        let mut resident: Vec<&str> = self.sessions.keys().map(String::as_str).collect();
        resident.sort_unstable();
        json!({
            "pid": std::process::id(),
            "uptime_ms": self.started.elapsed().as_millis() as u64,
            "completed_requests": self.completed,
            "failed_requests": self.failed,
            "resident_sessions": resident,
            "active_session": self.active_session,
            "max_response_bytes": MAX_RESPONSE_BYTES,
            "max_sessions": MAX_SESSIONS
        })
    }

    fn close_session(&mut self, id: &str, params: Option<Value>) -> Response {
        let params: CloseParams = match parse_params(params) {
            Ok(params) => params,
            Err(message) => return Response::error(id, "invalid_params", message),
        };
        if validate_session_name(&params.session).is_err() {
            return Response::error(id, "invalid_params", "invalid session name");
        }
        let closed = self.remove_session(&params.session);
        Response::success(id, json!({"session": params.session, "closed": closed}))
    }

    async fn fetch(&mut self, id: &str, params: Option<Value>) -> Response {
        let params: FetchParams = match parse_params(params) {
            Ok(params) => params,
            Err(message) => return Response::error(id, "invalid_params", message),
        };
        if let Err((code, message)) = validate_fetch_params(&params) {
            return Response::error(id, code, message);
        }

        if let Some(ref name) = params.session {
            if let Err(response) = self.ensure_session(id, name, params.storage_dir.clone()) {
                return response;
            }
            self.activate_for_navigation(name);
            let response = {
                let session = self.sessions.get_mut(name).expect("session was just created");
                execute_fetch(id, &mut session.page, &params).await
            };
            if let Some(session) = self.sessions.get(name) {
                session.context.save_session();
            }
            self.sequence += 1;
            if let Some(session) = self.sessions.get_mut(name) {
                session.last_used = self.sequence;
            }
            response
        } else {
            self.suspend_active();
            let context = Arc::new(self.build_context(
                format!("fetch-ephemeral-{}", self.sequence),
                None,
            ));
            let mut page = Page::new(format!("fetch-ephemeral-page-{}", self.sequence), context);
            let response = execute_fetch(id, &mut page, &params).await;
            page.suspend_js();
            self.sequence += 1;
            response
        }
    }

    fn ensure_session(&mut self, id: &str, name: &str, storage_dir: Option<PathBuf>) -> Result<(), Response> {
        if self.sessions.contains_key(name) {
            let expected = self.sessions[name].context.storage_dir.as_ref();
            if expected != storage_dir.as_ref() {
                return Err(Response::error(id, "invalid_params", "storage_dir differs from the resident session"));
            }
            return Ok(());
        }
        if self.sessions.len() == MAX_SESSIONS {
            let lru = self.sessions
                .iter()
                .min_by_key(|(_, session)| session.last_used)
                .map(|(name, _)| name.clone())
                .expect("full session map is non-empty");
            self.remove_session(&lru);
        }
        let context = Arc::new(self.build_context(
            format!("fetch-session-{name}"),
            storage_dir,
        ));
        let page = Page::new(format!("fetch-session-page-{name}"), context.clone());
        self.sessions.insert(name.to_string(), Session { context, page, last_used: self.sequence });
        Ok(())
    }

    fn build_context(&self, id: String, storage_dir: Option<PathBuf>) -> BrowserContext {
        let mut context = BrowserContext::with_storage_and_network(
            id,
            self.config.proxy.clone(),
            self.config.stealth,
            self.config.user_agent.clone(),
            storage_dir,
            self.config.allow_private_network,
        );
        if let Some(extension) = &self.config.extension {
            context = context.with_extension(extension.clone());
        }
        context
    }

    fn activate_for_navigation(&mut self, name: &str) {
        if self.active_session.as_deref() != Some(name) {
            self.suspend_active();
        }
        // navigate_with_wait creates a fresh realm. Do not resume a suspended
        // page here only to throw that isolate away during navigation.
        self.active_session = Some(name.to_string());
    }

    fn suspend_active(&mut self) {
        if let Some(name) = self.active_session.take() {
            if let Some(session) = self.sessions.get_mut(&name) {
                session.page.suspend_js();
            }
        }
    }

    fn remove_session(&mut self, name: &str) -> bool {
        if self.active_session.as_deref() == Some(name) {
            self.suspend_active();
        }
        if let Some(mut session) = self.sessions.remove(name) {
            session.page.suspend_js();
            session.context.save_session();
            true
        } else {
            false
        }
    }

    fn cleanup(&mut self) {
        self.suspend_active();
        for (_, mut session) in self.sessions.drain() {
            session.page.suspend_js();
            session.context.save_session();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[derive(Debug)]
struct Response {
    id: String,
    ok: bool,
    result: Option<Value>,
    error: Option<Value>,
}

impl Response {
    fn success(id: &str, result: Value) -> Self {
        Self { id: id.to_string(), ok: true, result: Some(result), error: None }
    }

    fn error(id: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            ok: false,
            result: None,
            error: Some(json!({"code": code, "message": message.into()})),
        }
    }

    fn to_value(&self) -> Value {
        if self.ok {
            json!({"v": PROTOCOL_VERSION, "id": self.id, "ok": true, "result": self.result})
        } else {
            json!({"v": PROTOCOL_VERSION, "id": self.id, "ok": false, "error": self.error})
        }
    }
}

fn parse_params<T: for<'de> Deserialize<'de>>(params: Option<Value>) -> Result<T, String> {
    serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|error| error.to_string())
}

fn validate_fetch_params(params: &FetchParams) -> Result<(), (&'static str, String)> {
    if params.url.len() > MAX_URL {
        return Err(("invalid_url", "url exceeds 4096 bytes".to_string()));
    }
    let parsed = url::Url::parse(&params.url)
        .map_err(|error| ("invalid_url", format!("invalid URL: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(("invalid_url", "url scheme must be http or https".to_string()));
    }
    if !matches!(params.format.as_str(), "html" | "text" | "links" | "markdown" | "accessibility") {
        return Err(("invalid_params", "format must be html, text, links, markdown, or accessibility".to_string()));
    }
    if !matches!(params.wait_until.as_str(), "load" | "domcontentloaded" | "networkidle0" | "networkidle" | "networkIdle" | "networkidle2") {
        return Err(("invalid_params", "unsupported wait_until value".to_string()));
    }
    if !(1_000..=MAX_TIMEOUT_MS).contains(&params.timeout_ms) {
        return Err(("invalid_params", "timeout_ms must be between 1000 and 120000".to_string()));
    }
    if params.settle_ms > MAX_TIMEOUT_MS {
        return Err(("invalid_params", "settle_ms must not exceed 120000".to_string()));
    }
    if params.selector.as_ref().is_some_and(|value| value.len() > MAX_SELECTOR) {
        return Err(("invalid_params", "selector exceeds 64 KiB".to_string()));
    }
    if params.eval.as_ref().is_some_and(|value| value.len() > MAX_EVAL) {
        return Err(("invalid_params", "eval exceeds 1 MiB".to_string()));
    }
    match (&params.session, &params.storage_dir) {
        (Some(name), Some(path)) => {
            validate_session_name(name).map_err(|message| ("invalid_params", message.to_string()))?;
            if !path.is_absolute() {
                return Err(("invalid_params", "storage_dir must be absolute".to_string()));
            }
        }
        (None, None) => {}
        _ => return Err(("invalid_params", "session and storage_dir must be supplied together".to_string())),
    }
    Ok(())
}

fn validate_session_name(name: &str) -> Result<(), &'static str> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid { Ok(()) } else { Err("invalid session name") }
}

async fn execute_fetch(id: &str, page: &mut Page, params: &FetchParams) -> Response {
    let timeout_duration = Duration::from_millis(params.timeout_ms);
    let wait_until = WaitUntil::from_str(&params.wait_until);
    match tokio::time::timeout(timeout_duration, page.navigate_with_wait(&params.url, wait_until)).await {
        Err(_) => return Response::error(id, "navigation_timeout", format!("navigation exceeded {}ms", params.timeout_ms)),
        Ok(Err(error)) => return Response::error(id, "navigation_failed", error.to_string()),
        Ok(Ok(())) => {}
    }

    if params.settle_ms > 0 {
        page.settle(params.settle_ms).await;
    }
    if let Some(expression) = &params.eval {
        let eval_result = match page.js.as_mut() {
            Some(js) => js.evaluate_with_timeout(expression, timeout_duration),
            None => return Response::error(id, "eval_failed", "page has no JavaScript runtime"),
        };
        if let Err(error) = eval_result {
            return Response::error(id, "eval_failed", error.to_string());
        }
        if params.settle_ms > 0 {
            page.settle(params.settle_ms).await;
        }
    }
    if let Some(selector) = &params.selector {
        if !wait_for_selector(page, selector, timeout_duration).await {
            return Response::error(id, "selector_timeout", format!("selector was not found within {}ms", params.timeout_ms));
        }
    }

    let body = match params.format.as_str() {
        "html" => dump_html(page),
        "text" => dump_text(page),
        "links" => dump_links(page),
        "markdown" => dump_markdown(page),
        "accessibility" => dump_accessibility(page),
        _ => unreachable!("format was validated"),
    };
    if body.len() > MAX_RESPONSE_BYTES {
        return Response::error(id, "output_too_large", format!("response body exceeds {MAX_RESPONSE_BYTES} bytes"));
    }
    let bytes = body.len();
    Response::success(id, json!({
        "url": page.url_string(),
        "title": page.title,
        "format": params.format,
        "body": body,
        "bytes": bytes,
        "session": params.session
    }))
}

async fn wait_for_selector(page: &Page, selector: &str, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let found = page.with_dom(|dom| dom.query_selector(selector).ok().flatten().is_some()).unwrap_or(false);
        if found {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn dump_html(page: &Page) -> String {
    page.with_dom(|dom| {
        if let Ok(Some(html)) = dom.query_selector("html") {
            format!("<!DOCTYPE html>\n{}", dom.outer_html(html))
        } else {
            dom.inner_html(dom.document())
        }
    }).unwrap_or_default()
}

fn dump_text(page: &Page) -> String {
    page.with_dom(|dom| {
        dom.query_selector("body")
            .ok()
            .flatten()
            .map(|body| dom.text_content(body).trim().to_string())
            .unwrap_or_default()
    }).unwrap_or_default()
}

fn dump_links(page: &Page) -> String {
    let base_url = page.url.clone();
    page.with_dom(|dom| {
        let mut rendered = Vec::new();
        for link in dom.query_selector_all("a").unwrap_or_default() {
            let Some(node) = dom.get_node(link) else { continue };
            let href = node.get_attribute("href").unwrap_or_default().to_string();
            let full_url = if matches!(href.as_str(), value if value.starts_with("http://") || value.starts_with("https://")) {
                href
            } else if let Some(base) = &base_url {
                base.join(&href).map(|url| url.to_string()).unwrap_or(href)
            } else {
                href
            };
            if !full_url.is_empty() {
                let text = dom.text_content(link);
                let text = text.trim();
                rendered.push(if text.is_empty() { full_url } else { format!("{full_url}\t{text}") });
            }
        }
        rendered.join("\n")
    }).unwrap_or_default()
}

fn dump_markdown(page: &mut Page) -> String {
    page.evaluate(obscura_browser::HTML_TO_MARKDOWN_JS)
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn dump_accessibility(page: &Page) -> String {
    let nodes = page
        .with_dom(obscura_cdp::domains::accessibility::build_ax_nodes)
        .unwrap_or_default();
    json!({ "nodes": nodes }).to_string()
}

enum ReadLine {
    Eof,
    Line(Vec<u8>),
    TooLarge,
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<ReadLine> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() { Ok(ReadLine::Eof) } else { Ok(ReadLine::Line(line)) };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let payload = newline.unwrap_or(available.len());
        let consumed = payload + usize::from(newline.is_some());
        let remaining = MAX_REQUEST_LINE.saturating_add(1).saturating_sub(line.len());
        let copy = payload.min(remaining);
        line.extend_from_slice(&available[..copy]);
        reader.consume(consumed);
        if line.len() > MAX_REQUEST_LINE {
            if newline.is_none() {
                discard_until_newline(reader).await?;
            }
            return Ok(ReadLine::TooLarge);
        }
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(ReadLine::Line(line));
        }
    }
}

async fn discard_until_newline<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<()> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map(|index| index + 1).unwrap_or(available.len());
        reader.consume(take);
        if newline.is_some() {
            return Ok(());
        }
    }
}

async fn write_response<W: AsyncWrite + Unpin>(writer: &mut W, response: &Response) -> std::io::Result<()> {
    let mut encoded = serde_json::to_vec(&response.to_value())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await
}

pub async fn run() {
    let config = WorkerConfig::from_env();
    let idle_timeout = config.idle_timeout;
    let mut worker = Worker::new(config);
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();

    loop {
        let read = tokio::time::timeout(idle_timeout, read_bounded_line(&mut reader)).await;
        let (response, shutdown) = match read {
            Err(_) => break,
            Ok(Err(error)) => {
                eprintln!("fetch worker stdin error: {error}");
                break;
            }
            Ok(Ok(ReadLine::Eof)) => break,
            Ok(Ok(ReadLine::TooLarge)) => (Response::error("", "invalid_request", "request line exceeds 1 MiB"), false),
            Ok(Ok(ReadLine::Line(line))) if line.is_empty() => continue,
            Ok(Ok(ReadLine::Line(line))) => worker.dispatch(&line).await,
        };
        if let Err(error) = write_response(&mut stdout, &response).await {
            eprintln!("fetch worker stdout error: {error}");
            break;
        }
        if shutdown {
            break;
        }
    }
    worker.cleanup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch_params() -> FetchParams {
        FetchParams {
            url: "https://example.com".to_string(),
            format: "text".to_string(),
            wait_until: "domcontentloaded".to_string(),
            timeout_ms: 30_000,
            settle_ms: 0,
            selector: None,
            eval: None,
            session: None,
            storage_dir: None,
        }
    }

    #[test]
    fn validates_fetch_boundaries() {
        let mut params = fetch_params();
        assert!(validate_fetch_params(&params).is_ok());
        params.timeout_ms = 999;
        assert_eq!(validate_fetch_params(&params).unwrap_err().0, "invalid_params");
        params.timeout_ms = 120_001;
        assert_eq!(validate_fetch_params(&params).unwrap_err().0, "invalid_params");
        params.timeout_ms = 30_000;
        params.url = "file:///tmp/a".to_string();
        assert_eq!(validate_fetch_params(&params).unwrap_err().0, "invalid_url");
    }

    #[test]
    fn validates_named_session_pair() {
        let mut params = fetch_params();
        params.session = Some("session-1".to_string());
        assert!(validate_fetch_params(&params).is_err());
        params.storage_dir = Some(PathBuf::from("/tmp/session-1"));
        assert!(validate_fetch_params(&params).is_ok());
        params.session = Some("../escape".to_string());
        assert!(validate_fetch_params(&params).is_err());
    }

    #[tokio::test]
    async fn rejects_protocol_version_without_exiting() {
        let config = WorkerConfig {
            proxy: None,
            stealth: false,
            user_agent: None,
            allow_private_network: false,
            idle_timeout: Duration::from_secs(1),
            extension: None,
        };
        let mut worker = Worker::new(config);
        let (response, shutdown) = worker.dispatch(br#"{"v":2,"id":"a","op":"hello"}"#).await;
        assert!(!response.ok);
        assert!(!shutdown);
        assert_eq!(response.error.unwrap()["code"], "unsupported_version");
    }

    #[test]
    fn response_envelopes_are_stable() {
        let success = Response::success("7", json!({"x": 1})).to_value();
        assert_eq!(success, json!({"v":1,"id":"7","ok":true,"result":{"x":1}}));
        let error = Response::error("8", "invalid_request", "bad").to_value();
        assert_eq!(error, json!({"v":1,"id":"8","ok":false,"error":{"code":"invalid_request","message":"bad"}}));
    }

    #[tokio::test]
    async fn bounded_reader_recovers_after_oversized_line() {
        let mut input = vec![b'x'; MAX_REQUEST_LINE + 1];
        input.extend_from_slice(b"\n{}\n");
        let mut reader = BufReader::new(input.as_slice());
        assert!(matches!(read_bounded_line(&mut reader).await.unwrap(), ReadLine::TooLarge));
        match read_bounded_line(&mut reader).await.unwrap() {
            ReadLine::Line(line) => assert_eq!(line, b"{}"),
            _ => panic!("expected second line"),
        }
    }
}
