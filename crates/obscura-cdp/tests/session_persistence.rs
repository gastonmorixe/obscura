//! End-to-end integration test for session persistence (`--storage-dir`).
//!
//! No external network: spins up a `tokio::net::TcpListener` on a random
//! local port and serves two pages over plain HTTP:
//!
//! - `/set` sets a `Set-Cookie` header, then JS writes two keys into
//!   `localStorage`.
//! - `/check` is an empty page whose URL is the same origin, used as a
//!   navigation target so a second run can read both cookie + localStorage
//!   for that origin.
//!
//! Run 1 navigates to `/set` and lets `save_session()` flush.
//! Run 2 spins up a *new* `CdpContext` pointing at the *same* `storage_dir`
//! and verifies:
//!   - `document.cookie` reports the cookie from run 1
//!   - `localStorage.getItem(...)` returns the values from run 1
//!
//! This is the contract callers care about for staying logged in.
//!
//! The private-IP guard is bypassed by setting
//! `OBSCURA_ALLOW_PRIVATE_NETWORK=1`, matching the other CDP tests.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve `/set` once and `/check` once, then exit. Returns the base URL.
async fn serve_two_requests() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);

                let (status, extra_headers, body) = if req.starts_with("GET /set") {
                    (
                        "200 OK",
                        // Set a session cookie + a longer-lived one so we can
                        // verify both shapes round-trip.
                        "Set-Cookie: sid=abc123; Path=/\r\n\
                         Set-Cookie: pref=dark; Path=/; Max-Age=3600\r\n",
                        r#"<!doctype html>
<html><body>
<script>
  localStorage.setItem('user', 'gaston');
  localStorage.setItem('theme', 'dark');
</script>
</body></html>"#,
                    )
                } else {
                    (
                        "200 OK",
                        "",
                        r#"<!doctype html><html><body>check</body></html>"#,
                    )
                };

                let resp = format!(
                    "HTTP/1.1 {status}\r\n\
                     Content-Type: text/html\r\n\
                     Content-Length: {}\r\n\
                     {extra_headers}\
                     Connection: close\r\n\
                     \r\n{body}",
                    body.len()
                );
                socket.write_all(resp.as_bytes()).await.unwrap();
            });
        }
    });
    format!("http://{addr}")
}

/// Dispatch helper that asserts no error and returns the result value.
async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session_id: &str,
) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session_id.to_string()),
        },
        ctx,
    )
    .await;
    assert!(
        resp.error.is_none(),
        "CDP {method} failed: {:?}",
        resp.error
    );
    resp.result.unwrap_or_else(|| json!({}))
}

/// Attach to a freshly-created page and return the session id mapping.
async fn create_attached_page(ctx: &mut CdpContext) -> String {
    let page_id = ctx.create_page();
    let session_id = format!("session-{page_id}");
    ctx.sessions.insert(session_id.clone(), page_id);
    // Runtime.enable bootstraps the executionContextCreated machinery for
    // this session, which Runtime.evaluate consults.
    cdp(ctx, 1, "Runtime.enable", json!({}), &session_id).await;
    session_id
}

#[tokio::test(flavor = "current_thread")]
async fn session_persists_cookies_and_localstorage_across_contexts() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");

    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().to_path_buf();

    let url = serve_two_requests().await;
    let set_url = format!("{url}/set");
    let check_url = format!("{url}/check");

    // ── run 1: write cookie + localStorage, save ────────────────────────
    {
        let mut ctx = CdpContext::new_with_storage(None, false, None, Some(storage_dir.clone()));
        let session_id = create_attached_page(&mut ctx).await;

        cdp(
            &mut ctx,
            10,
            "Page.navigate",
            json!({"url": set_url}),
            &session_id,
        )
        .await;

        // Confirm the in-process write went through before we flush.
        let in_proc = cdp(
            &mut ctx,
            11,
            "Runtime.evaluate",
            json!({
                "expression": "JSON.stringify({ user: localStorage.getItem('user'), theme: localStorage.getItem('theme'), len: localStorage.length })",
                "returnByValue": true,
            }),
            &session_id,
        )
        .await;
        assert_eq!(
            in_proc["result"]["value"], r#"{"user":"gaston","theme":"dark","len":2}"#,
            "run 1 in-process localStorage state",
        );

        // Same single exit point the CDP server uses.
        ctx.default_context.save_session();
    }

    // ── on-disk shape sanity check ──────────────────────────────────────
    let cookies_path = storage_dir.join("cookies.json");
    let ls_path = storage_dir.join("localstorage.json");
    assert!(cookies_path.exists(), "cookies.json not written");
    assert!(ls_path.exists(), "localstorage.json not written");

    let cookies_raw = std::fs::read_to_string(&cookies_path).unwrap();
    assert!(
        cookies_raw.contains("sid") && cookies_raw.contains("abc123"),
        "cookies.json missing session cookie: {cookies_raw}",
    );
    assert!(
        cookies_raw.contains("pref") && cookies_raw.contains("dark"),
        "cookies.json missing pref cookie: {cookies_raw}",
    );

    let ls_raw = std::fs::read_to_string(&ls_path).unwrap();
    let ls_json: serde_json::Value = serde_json::from_str(&ls_raw).unwrap();
    // The origin includes the random port, so we match on the inner bucket.
    let bucket = ls_json
        .as_object()
        .and_then(|o| o.values().next())
        .and_then(|v| v.as_object())
        .expect("localstorage.json origin bucket missing");
    assert_eq!(bucket.get("user").and_then(|v| v.as_str()), Some("gaston"));
    assert_eq!(bucket.get("theme").and_then(|v| v.as_str()), Some("dark"));

    // ── run 2: brand-new context, same storage_dir, verify load ─────────
    {
        let mut ctx = CdpContext::new_with_storage(None, false, None, Some(storage_dir.clone()));
        let session_id = create_attached_page(&mut ctx).await;

        // Navigate to /check (different path, same origin) so the page's
        // `state.url` matches the origin we wrote to. localStorage and
        // cookies are both origin-scoped, so navigating to ANY path on the
        // same authority is enough.
        cdp(
            &mut ctx,
            20,
            "Page.navigate",
            json!({"url": check_url}),
            &session_id,
        )
        .await;

        let restored = cdp(
            &mut ctx,
            21,
            "Runtime.evaluate",
            json!({
                "expression": "JSON.stringify({ user: localStorage.getItem('user'), theme: localStorage.getItem('theme'), cookie: document.cookie })",
                "returnByValue": true,
            }),
            &session_id,
        )
        .await;
        let body = restored["result"]["value"].as_str().unwrap_or("");
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            parsed["user"].as_str(),
            Some("gaston"),
            "user lost across runs"
        );
        assert_eq!(
            parsed["theme"].as_str(),
            Some("dark"),
            "theme lost across runs"
        );
        let cookie = parsed["cookie"].as_str().unwrap_or("");
        assert!(
            cookie.contains("sid=abc123"),
            "session cookie lost across runs: {cookie}",
        );
        assert!(
            cookie.contains("pref=dark"),
            "pref cookie lost across runs: {cookie}",
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn localstorage_is_origin_scoped_in_memory_without_storage_dir() {
    // Same in-memory store behaviour even when --storage-dir is NOT set:
    // a single CdpContext keeps localStorage across navigations within
    // one origin, but the data lives only as long as the process.
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");

    let url = serve_two_requests().await;
    let set_url = format!("{url}/set");
    let check_url = format!("{url}/check");

    let mut ctx = CdpContext::new();
    let session_id = create_attached_page(&mut ctx).await;

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": set_url}),
        &session_id,
    )
    .await;
    cdp(
        &mut ctx,
        2,
        "Page.navigate",
        json!({"url": check_url}),
        &session_id,
    )
    .await;

    let after_nav = cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({
            "expression": "JSON.stringify({ user: localStorage.getItem('user'), theme: localStorage.getItem('theme') })",
            "returnByValue": true,
        }),
        &session_id,
    )
    .await;
    assert_eq!(
        after_nav["result"]["value"], r#"{"user":"gaston","theme":"dark"}"#,
        "localStorage did not survive same-origin navigation in memory",
    );
}
