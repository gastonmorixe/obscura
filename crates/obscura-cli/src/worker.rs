mod fetch_worker;

use std::sync::Arc;

use obscura_browser::{BrowserContext, Page, WaitUntil};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd")]
enum WorkerCommand {
    #[serde(rename = "navigate")]
    Navigate {
        url: String,
        #[serde(default, rename = "waitUntil")]
        wait_until: NavigationWaitUntil,
    },
    #[serde(rename = "evaluate")]
    Evaluate { expression: String },
    #[serde(rename = "title")]
    Title,
    #[serde(rename = "dump_html")]
    DumpHtml,
    #[serde(rename = "dump_text")]
    DumpText,
    #[serde(rename = "shutdown")]
    Shutdown,
}

/// Keep the wire protocol strict: WaitUntil::from_str falls back to Load.
#[derive(Debug, Default, Deserialize)]
enum NavigationWaitUntil {
    #[default]
    #[serde(rename = "load")]
    Load,
    #[serde(rename = "domcontentloaded")]
    DomContentLoaded,
    #[serde(rename = "networkidle0")]
    NetworkIdle0,
    #[serde(rename = "networkidle2")]
    NetworkIdle2,
}

impl From<NavigationWaitUntil> for WaitUntil {
    fn from(value: NavigationWaitUntil) -> Self {
        match value {
            NavigationWaitUntil::Load => Self::Load,
            NavigationWaitUntil::DomContentLoaded => Self::DomContentLoaded,
            NavigationWaitUntil::NetworkIdle0 => Self::NetworkIdle0,
            NavigationWaitUntil::NetworkIdle2 => Self::NetworkIdle2,
        }
    }
}

#[derive(Debug, Serialize)]
struct WorkerResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl WorkerResponse {
    fn success(result: serde_json::Value) -> Self {
        WorkerResponse { ok: true, result: Some(result), error: None }
    }
    fn error(msg: String) -> Self {
        WorkerResponse { ok: false, result: None, error: Some(msg) }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .init();

    if std::env::args().skip(1).any(|arg| arg == "--fetch-protocol") {
        fetch_worker::run().await;
        return;
    }

    let proxy = std::env::var("OBSCURA_PROXY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let stealth = std::env::var("OBSCURA_STEALTH")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    let obey_robots = std::env::var("OBSCURA_OBEY_ROBOTS")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    let user_agent = std::env::var("OBSCURA_USER_AGENT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut context =
        BrowserContext::with_full_options("worker".to_string(), proxy, stealth, user_agent);
    context.obey_robots = obey_robots;
    let context = Arc::new(context);
    let mut page = Page::new("page-1".to_string(), context);

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("Worker stdin error: {}", e);
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let cmd: WorkerCommand = match serde_json::from_str(trimmed) {
            Ok(c) => c,
            Err(e) => {
                let resp = WorkerResponse::error(format!("Invalid command: {}", e));
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.flush().await;
                continue;
            }
        };

        let resp = match cmd {
            WorkerCommand::Navigate { url, wait_until } => {
                match page.navigate_with_wait(&url, wait_until.into()).await {
                    Ok(()) => {
                        // Navigation clears the event list and records the main
                        // Document response before scripts and child frames.
                        // Its status is the final response after HTTP redirects.
                        // Zero denotes a blocked request, not an HTTP response.
                        let status = page.network_events.iter()
                            .find(|event| event.resource_type == "Document")
                            .filter(|event| {
                                event.status != 0
                                    && (event.url.starts_with("http://") || event.url.starts_with("https://"))
                            })
                            .map(|event| event.status);
                        WorkerResponse::success(serde_json::json!({
                            "title": page.title,
                            "url": page.url_string(),
                            "status": status,
                        }))
                    },
                    Err(e) => WorkerResponse::error(e.to_string()),
                }
            }
            WorkerCommand::Evaluate { expression } => {
                // Await promise-returning expressions so async IIFEs resolve
                // before serialization. Previously the sync path serialized an
                // unresolved Promise as `{}`, making single-invocation flows
                // that call async app APIs impossible (issue #693). A 30s cap
                // matches the CDP await timeout so a never-settling promise
                // cannot hang the worker.
                let result = match page
                    .evaluate_for_cdp_with_timeout(&expression, true, true, 30_000)
                    .await
                {
                    Ok(info) => match info.value {
                        Some(v) => v,
                        None => serde_json::Value::String(info.description),
                    },
                    Err(_) => serde_json::Value::Null,
                };
                WorkerResponse::success(result)
            }
            WorkerCommand::Title => {
                WorkerResponse::success(serde_json::json!(page.title))
            }
            WorkerCommand::DumpHtml => {
                let html = page.with_dom(|dom| {
                    if let Ok(Some(html_node)) = dom.query_selector("html") {
                        dom.outer_html(html_node)
                    } else {
                        dom.inner_html(dom.document())
                    }
                }).unwrap_or_default();
                WorkerResponse::success(serde_json::json!(html))
            }
            WorkerCommand::DumpText => {
                let text = page.with_dom(|dom| {
                    if let Ok(Some(body)) = dom.query_selector("body") {
                        collapse_dump_text_whitespace(extract_worker_text(dom, body).trim())
                    } else {
                        String::new()
                    }
                }).unwrap_or_default();
                WorkerResponse::success(serde_json::json!(text))
            }
            WorkerCommand::Shutdown => {
                let resp = WorkerResponse::success(serde_json::json!("bye"));
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.flush().await;
                break;
            }
        };

        let mut out = serde_json::to_string(&resp).unwrap();
        out.push('\n');
        let _ = stdout.write_all(out.as_bytes()).await;
        let _ = stdout.flush().await;
    }
}

/// Match the CLI walker's block-newline text for --dump text (issue #362),
/// so worker scrape output stays identical to the CLI path.
fn extract_worker_text(dom: &obscura_dom::DomTree, node_id: obscura_dom::NodeId) -> String {
    use obscura_dom::NodeData;
    enum Work {
        Visit(obscura_dom::NodeId),
        Newline,
    }
    const MAX_NODES: usize = 5_000_000;
    fn push_children(dom: &obscura_dom::DomTree, id: obscura_dom::NodeId, stack: &mut Vec<Work>) {
        for child_id in dom.children(id).into_iter().rev() {
            stack.push(Work::Visit(child_id));
        }
    }
    fn append_segment(result: &mut String, pending_space: &mut bool, contents: &str) {
        let trimmed = contents.trim_matches(|c: char| matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' '));
        if trimmed.is_empty() {
            if contents.chars().any(|c| matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' ')) {
                *pending_space = true;
            }
            return;
        }
        let begins_with_space = contents.chars().next().is_some_and(|c| c.is_whitespace());
        let ends_with_space = result.chars().next_back().is_some_and(char::is_whitespace);
        if (*pending_space || begins_with_space) && !result.is_empty() && !ends_with_space {
            result.push(' ');
        }
        result.push_str(trimmed);
        *pending_space = contents.chars().next_back().is_some_and(|c| c.is_whitespace());
    }
    let mut result = String::new();
    let mut pending_space = false;
    let mut stack: Vec<Work> = vec![Work::Visit(node_id)];
    let mut visited = 0usize;
    while let Some(work) = stack.pop() {
        let id = match work {
            Work::Newline => {
                result.push('\n');
                pending_space = false;
                continue;
            }
            Work::Visit(id) => id,
        };
        visited += 1;
        if visited > MAX_NODES {
            break;
        }
        let Some(node) = dom.get_node(id) else {
            continue;
        };
        match &node.data {
            NodeData::Text { contents } => append_segment(&mut result, &mut pending_space, contents),
            NodeData::Element { name, .. } => {
                let tag = name.local.as_ref();
                if matches!(
                    tag,
                    "script" | "style" | "noscript" | "nav" | "header" | "footer" | "aside"
                ) {
                    continue;
                }
                let is_block = matches!(
                    tag,
                    "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "tr"
                        | "br" | "hr" | "blockquote" | "pre" | "section" | "article"
                        | "header" | "footer" | "nav" | "main" | "aside" | "figure"
                        | "figcaption" | "table" | "thead" | "tbody" | "tfoot" | "dl"
                        | "dt" | "dd" | "ul" | "ol"
                );
                if is_block {
                    result.push('\n');
                    pending_space = false;
                    stack.push(Work::Newline);
                }
                push_children(dom, id, &mut stack);
            }
            _ => push_children(dom, id, &mut stack),
        }
    }
    result
}

/// Generic whitespace cleanup for DumpText. Whitespace only: strips trailing
/// spaces per line and collapses 3+ consecutive newlines to 2 (max 1 blank
/// line). Single blank lines stay. No content is removed.
fn collapse_dump_text_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0usize;
    for raw_line in text.split('\n') {
        let line = raw_line.trim_end();
        if line.is_empty() {
            blanks += 1;
            if blanks <= 1 {
                out.push('\n');
            }
        } else {
            blanks = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}
