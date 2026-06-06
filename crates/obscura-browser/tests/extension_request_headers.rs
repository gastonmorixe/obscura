//! End-to-end test of the extension request-header pre-pass: a synthetic
//! WebExtension registers a `declarativeNetRequest` `modifyHeaders` rule (and
//! a blocking MV2 `webRequest.onBeforeSendHeaders` listener) that sets a
//! `Referer` on the top-level document, exactly the way Bypass Paywalls Clean
//! unlocks WSJ. We drive the real chrome-shim + V8 runtime through
//! `Page::resolve_extension_request_headers` and assert the header is
//! resolved for in-scope URLs and absent for out-of-scope ones.
//!
//! This is the regression guard for the "obscura must let the extension do
//! its work" fix: it exercises the JS shim's rule registry, the Rust DNR
//! harvest/parse, and the resolver in one shot, with no network.

use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};
use obscura_ext::{Bundle, ExtensionRuntime};

/// Write a synthetic unpacked MV2 extension that targets wsj.com and sets
/// `Referer: https://www.drudgereport.com/` via BOTH mechanisms BPC uses
/// (DNR session rule + blocking onBeforeSendHeaders), so the test proves
/// either path lands the header.
fn build_paywall_ext(dir: &std::path::Path) {
    std::fs::write(
        dir.join("manifest.json"),
        r#"{
          "manifest_version": 2,
          "name": "SynthPaywall",
          "version": "1.0",
          "permissions": [
            "*://*.wsj.com/*", "webRequest", "webRequestBlocking", "declarativeNetRequest"
          ],
          "background": { "scripts": ["bg.js"] }
        }"#,
    )
    .unwrap();

    std::fs::write(
        dir.join("bg.js"),
        r#"
        // MV3-style declarative rule.
        chrome.declarativeNetRequest.updateSessionRules({
          addRules: [{
            id: 1, priority: 1,
            action: { type: "modifyHeaders", requestHeaders: [
              { header: "Referer", operation: "set", value: "https://www.drudgereport.com/" }
            ]},
            condition: {
              urlFilter: "||wsj.com",
              resourceTypes: ["main_frame", "sub_frame", "xmlhttprequest", "script"]
            }
          }]
        });

        // MV2-style blocking listener (sets a User-Agent so we can also
        // assert the onBeforeSendHeaders path is wired).
        chrome.webRequest.onBeforeSendHeaders.addListener(function (details) {
          var h = details.requestHeaders || [];
          h.push({ name: "X-Obscura-Test", value: "mv2-listener-ran" });
          return { requestHeaders: h };
        }, { urls: ["*://*.wsj.com/*"] }, ["blocking", "requestHeaders"]);
        "#,
    )
    .unwrap();
}

fn runtime_from_dir(dir: &std::path::Path) -> Arc<ExtensionRuntime> {
    let bundle = Bundle::load(dir).expect("bundle load");
    Arc::new(ExtensionRuntime::new(Arc::new(bundle)))
}

#[tokio::test(flavor = "current_thread")]
async fn resolves_referer_for_in_scope_wsj_url() {
    let tmp = tempdir::TempDir::new("obscura-reqhdr-test").unwrap();
    build_paywall_ext(tmp.path());
    let ext = runtime_from_dir(tmp.path());

    let ctx = Arc::new(
        BrowserContext::with_options("test".into(), None, false).with_extension(ext),
    );
    let page = Page::new("p".into(), ctx);

    let hdrs = page
        .resolve_extension_request_headers(
            "https://www.wsj.com/tech/ai/meta-keeps-delaying-the-release-f8569c8c",
        )
        .await;

    // DNR modifyHeaders rule -> Referer set.
    assert_eq!(
        hdrs.get("referer").map(String::as_str),
        Some("https://www.drudgereport.com/"),
        "DNR Referer rule should resolve for wsj.com; got {hdrs:?}"
    );
    // MV2 blocking onBeforeSendHeaders listener ran and added its header.
    assert_eq!(
        hdrs.get("X-Obscura-Test").map(String::as_str),
        Some("mv2-listener-ran"),
        "onBeforeSendHeaders listener should have run; got {hdrs:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn no_headers_for_out_of_scope_url() {
    let tmp = tempdir::TempDir::new("obscura-reqhdr-test2").unwrap();
    build_paywall_ext(tmp.path());
    let ext = runtime_from_dir(tmp.path());

    let ctx = Arc::new(
        BrowserContext::with_options("test".into(), None, false).with_extension(ext),
    );
    let page = Page::new("p".into(), ctx);

    // nytimes.com is outside the extension's host_permissions, so the
    // pre-pass should not even run (matches_url gate) -> no headers.
    let hdrs = page
        .resolve_extension_request_headers("https://www.nytimes.com/some/article")
        .await;
    assert!(
        hdrs.is_empty(),
        "out-of-scope URL should resolve no extension headers; got {hdrs:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn no_extension_means_no_headers() {
    let ctx = Arc::new(BrowserContext::with_options("test".into(), None, false));
    let page = Page::new("p".into(), ctx);
    let hdrs = page
        .resolve_extension_request_headers("https://www.wsj.com/x")
        .await;
    assert!(hdrs.is_empty());
}
