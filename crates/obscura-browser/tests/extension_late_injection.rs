//! Late content-script injection: the extension idle phase runs after
//! the document's own scripts, so injected content scripts see page-built
//! DOM instead of the empty init_js stub. Also covers static manifest
//! `content_scripts` with run_at ordering. Served over loopback.

use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};
use obscura_ext::{Bundle, ExtensionRuntime};

fn runtime_from_dir(dir: &std::path::Path) -> Arc<ExtensionRuntime> {
    let bundle = Bundle::load(dir).expect("bundle load");
    Arc::new(ExtensionRuntime::new(Arc::new(bundle)))
}

fn late_injection_ext(dir: &std::path::Path) {
    std::fs::write(
        dir.join("manifest.json"),
        r#"{
          "manifest_version": 2,
          "name": "LateInject",
          "version": "1.0",
          "permissions": ["*://*/*"],
          "background": { "scripts": ["bg.js"] },
          "content_scripts": [
            { "matches": ["*://*/*"], "js": ["early.js"], "run_at": "document_start" },
            { "matches": ["*://*/*"], "js": ["late.js"], "run_at": "document_idle" },
            { "matches": ["*://other.test/*"], "js": ["skipped.js"], "run_at": "document_idle" }
          ]
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("bg.js"),
        r#"
        chrome.tabs.onUpdated.addListener(function (tabId, changeInfo) {
          if (changeInfo.status === "complete") {
            setTimeout(function () {
              chrome.tabs.executeScript(tabId, { file: "dynamic.js" });
            }, 0);
          }
        });
        "#,
    )
    .unwrap();
    // Dynamic injection via onUpdated + timer drain: must see the paywall node.
    std::fs::write(
        dir.join("dynamic.js"),
        r#"
        globalThis.__dynamicSawPaywall = !!document.querySelector(".paywall");
        "#,
    )
    .unwrap();
    // Static document_start must run even though it cannot see late DOM.
    std::fs::write(
        dir.join("early.js"),
        r#"globalThis.__staticOrder = (globalThis.__staticOrder || []).concat(["early"]);"#,
    )
    .unwrap();
    // Static document_idle must see the paywall node.
    std::fs::write(
        dir.join("late.js"),
        r#"
        globalThis.__staticOrder = (globalThis.__staticOrder || []).concat(["late"]);
        globalThis.__staticSawPaywall = !!document.querySelector(".paywall");
        "#,
    )
    .unwrap();
    std::fs::write(dir.join("skipped.js"), r#"globalThis.__skippedRan = true;"#).unwrap();
}

async fn serve_paywall_page() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // The paywall node is built by the page's own script, so only a
    // post-parse injection can see it.
    let body = "<html><body><div id=\"slot\"></div><script>document.getElementById(\"slot\").innerHTML = '<div class=\"paywall\">locked</div>';</script></body></html>";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/article", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let n = socket.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..n]);
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    url
}

#[tokio::test(flavor = "current_thread")]
async fn late_injection_sees_page_built_dom() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let tmp_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp");
    std::fs::create_dir_all(&tmp_root).unwrap();
    let tmp = tempfile::Builder::new()
        .prefix("obscura-late-inject")
        .tempdir_in(&tmp_root)
        .unwrap();
    late_injection_ext(tmp.path());

    let ctx = Arc::new(
        BrowserContext::with_storage_and_network(
            "late-inject".into(),
            None,
            false,
            None,
            None,
            true,
        )
        .with_extension(runtime_from_dir(tmp.path())),
    );
    let mut page = Page::new("p".into(), ctx);
    let url = serve_paywall_page().await;
    page.navigate(&url).await.expect("navigate");

    assert_eq!(
        page.evaluate("globalThis.__dynamicSawPaywall"),
        serde_json::json!(true),
        "timer-drained dynamic injection must see the page-script-built paywall node"
    );
    assert_eq!(
        page.evaluate("globalThis.__staticOrder"),
        serde_json::json!(["early", "late"]),
        "static scripts must run in run_at order"
    );
    assert_eq!(
        page.evaluate("globalThis.__staticSawPaywall"),
        serde_json::json!(true),
        "document_idle static script must see the page-script-built paywall node"
    );
    assert_eq!(
        page.evaluate("typeof globalThis.__skippedRan"),
        serde_json::json!("undefined"),
        "out-of-scope static script must not run"
    );
}
