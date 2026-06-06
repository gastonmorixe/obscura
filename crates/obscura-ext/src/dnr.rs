//! `chrome.declarativeNetRequest` rule model + matching, scoped to the
//! subset Obscura honours today: `modifyHeaders` actions that rewrite
//! *request* headers on the main-document fetch.
//!
//! Why only this subset? Real paywall-bypass extensions (Bypass Paywalls
//! Clean and friends) unlock article bodies by registering a session rule
//! that sets a `Referer` / `User-Agent` / `Cookie` header on the top-level
//! navigation. WSJ, for example, serves the full article only when the
//! request carries `Referer: https://www.drudgereport.com/`. The extension
//! commits that intent through
//! `chrome.declarativeNetRequest.updateSessionRules({addRules:[...]})`. If
//! Obscura treats that call as a no-op (as it used to), the header never
//! reaches the network layer and the page comes back truncated.
//!
//! We deliberately do NOT implement `block` / `redirect` / `upgradeScheme`
//! here — the existing `dnr_block_substrings` shortcut in `state.rs` covers
//! blocking, and redirects aren't needed for the paywall case. `response`
//! header modification is also out of scope (Obscura's dump pipeline reads
//! the response after the fact; rewriting response headers wouldn't change
//! what the server already sent).
//!
//! URL matching follows the DNR `urlFilter` mini-syntax
//! (<https://developer.chrome.com/docs/extensions/reference/api/declarativeNetRequest#url-filter-syntax>):
//!
//! - `*`  : wildcard, matches any run of characters
//! - `|`  : left/right anchor (start/end of URL)
//! - `||` : domain-name anchor (start of a (sub-)domain)
//! - `^`  : separator — anything that is not a letter, digit, `_`, `-`,
//!          `.`, or `%`, and also matches the end of the URL
//!
//! Matching is case-insensitive by default (`isUrlFilterCaseSensitive`
//! defaults to false), which is what BPC relies on. `regexFilter` is also
//! supported for the common anchored-prefix case via the `regex` crate.

use serde_json::Value;

/// One `modifyHeaders` operation on a single request header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderOp {
    /// Lower-cased header name (HTTP header names are case-insensitive).
    pub header: String,
    /// `set`, `append`, or `remove`.
    pub operation: HeaderOperation,
    /// Required for `set` / `append`; ignored for `remove`.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderOperation {
    Set,
    Append,
    Remove,
}

impl HeaderOperation {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "set" => Some(Self::Set),
            "append" => Some(Self::Append),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }
}

/// A DNR rule reduced to what Obscura acts on: a URL condition plus a list
/// of request-header modifications. Rules whose action isn't
/// `modifyHeaders`, or that carry no `requestHeaders`, are dropped at parse
/// time (they return `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderRule {
    /// Developer-assigned rule id (used for dedup / removal).
    pub id: i64,
    pub priority: i64,
    pub condition: UrlCondition,
    pub request_headers: Vec<HeaderOp>,
}

/// The matching half of a DNR rule, restricted to the fields BPC uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlCondition {
    /// Parsed `urlFilter` (mutually exclusive with `regex`).
    pub url_filter: Option<UrlFilter>,
    /// Parsed `regexFilter`.
    pub regex: Option<String>,
    /// `resourceTypes`. Empty = "all types except main_frame" per spec, but
    /// because Obscura only ever evaluates these against the top-level
    /// document fetch we treat empty as "match" (see `matches`).
    pub resource_types: Vec<String>,
    /// Lower-cased `requestDomains`, if present.
    pub request_domains: Vec<String>,
    pub excluded_request_domains: Vec<String>,
}

impl HeaderRule {
    /// Parse a single DNR rule JSON object. Returns `None` for any rule we
    /// don't model (non-`modifyHeaders` action, no request-header ops, or
    /// an unparseable condition).
    pub fn from_json(v: &Value) -> Option<Self> {
        let action = v.get("action")?;
        let action_type = action.get("type").and_then(|t| t.as_str())?;
        if action_type != "modifyHeaders" {
            return None;
        }

        let mut request_headers = Vec::new();
        if let Some(arr) = action.get("requestHeaders").and_then(|h| h.as_array()) {
            for h in arr {
                let header = h.get("header").and_then(|s| s.as_str())?;
                let op = h.get("operation").and_then(|s| s.as_str())?;
                let operation = HeaderOperation::from_str(op)?;
                let value = h
                    .get("value")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                // `set` / `append` require a value; skip malformed ops.
                if matches!(operation, HeaderOperation::Set | HeaderOperation::Append)
                    && value.is_none()
                {
                    continue;
                }
                request_headers.push(HeaderOp {
                    header: header.to_ascii_lowercase(),
                    operation,
                    value,
                });
            }
        }
        if request_headers.is_empty() {
            return None;
        }

        let id = v.get("id").and_then(|i| i.as_i64()).unwrap_or(0);
        let priority = v.get("priority").and_then(|i| i.as_i64()).unwrap_or(1);

        let cond_v = v.get("condition");
        let condition = parse_condition(cond_v);

        Some(HeaderRule {
            id,
            priority,
            condition,
            request_headers,
        })
    }

    /// Does this rule apply to `url` requested as `resource_type`
    /// (e.g. `"main_frame"`)?
    pub fn matches(&self, url: &str, resource_type: &str) -> bool {
        self.condition.matches(url, resource_type)
    }
}

fn parse_condition(v: Option<&Value>) -> UrlCondition {
    let Some(v) = v else {
        return UrlCondition {
            url_filter: None,
            regex: None,
            resource_types: Vec::new(),
            request_domains: Vec::new(),
            excluded_request_domains: Vec::new(),
        };
    };
    let url_filter = v
        .get("urlFilter")
        .and_then(|s| s.as_str())
        .map(UrlFilter::parse);
    let regex = v
        .get("regexFilter")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let resource_types = string_array(v.get("resourceTypes"));
    let request_domains = lower_string_array(v.get("requestDomains"));
    let excluded_request_domains = lower_string_array(v.get("excludedRequestDomains"));
    UrlCondition {
        url_filter,
        regex,
        resource_types,
        request_domains,
        excluded_request_domains,
    }
}

fn string_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn lower_string_array(v: Option<&Value>) -> Vec<String> {
    string_array(v)
        .into_iter()
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

impl UrlCondition {
    pub fn matches(&self, url: &str, resource_type: &str) -> bool {
        // resourceTypes: empty means "all types except main_frame" per the
        // DNR spec. But Obscura only evaluates header rules against the
        // top-level document fetch, and BPC always lists main_frame
        // explicitly, so: empty => match (be permissive), non-empty =>
        // require membership.
        if !self.resource_types.is_empty()
            && !self.resource_types.iter().any(|t| t == resource_type)
        {
            return false;
        }

        let host = host_of(url);

        if !self.request_domains.is_empty() {
            let ok = self
                .request_domains
                .iter()
                .any(|d| domain_matches(&host, d));
            if !ok {
                return false;
            }
        }
        if self
            .excluded_request_domains
            .iter()
            .any(|d| domain_matches(&host, d))
        {
            return false;
        }

        if let Some(uf) = &self.url_filter {
            if !uf.matches(url) {
                return false;
            }
        }
        if let Some(rx) = &self.regex {
            match regex::Regex::new(rx) {
                Ok(re) => {
                    if !re.is_match(url) {
                        return false;
                    }
                }
                // An unparseable regex matches nothing (Chrome would have
                // rejected the rule at load).
                Err(_) => return false,
            }
        }
        true
    }
}

/// `host` matches DNR-domain `d` if it equals `d` or is a sub-domain of it.
fn domain_matches(host: &str, d: &str) -> bool {
    host == d || host.ends_with(&format!(".{d}"))
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_default()
}

/// A compiled `urlFilter`. We tokenize into anchors + literal/`*`/`^`
/// segments and match greedily. Case-insensitive (lower-cased on both
/// sides) to mirror the `isUrlFilterCaseSensitive=false` default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlFilter {
    /// `|` at start: URL must begin here.
    anchor_start: bool,
    /// `||` at start: must begin at a (sub-)domain boundary.
    domain_anchor: bool,
    /// `|` at end: URL must end here.
    anchor_end: bool,
    /// Segments between `*` wildcards. Each segment is a run that must
    /// appear in order; an empty vec with no anchors matches everything.
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    /// Literal text (already lower-cased). `^` separators are represented
    /// inline as a sentinel split: we store the literal pieces and require a
    /// separator char (or end) between them.
    parts: Vec<String>,
}

impl UrlFilter {
    pub fn parse(raw: &str) -> Self {
        let mut s = raw;
        let mut domain_anchor = false;
        let mut anchor_start = false;
        if let Some(rest) = s.strip_prefix("||") {
            domain_anchor = true;
            s = rest;
        } else if let Some(rest) = s.strip_prefix('|') {
            anchor_start = true;
            s = rest;
        }
        let mut anchor_end = false;
        if let Some(rest) = s.strip_suffix('|') {
            anchor_end = true;
            s = rest;
        }
        let lower = s.to_ascii_lowercase();
        let segments = lower
            .split('*')
            .map(|seg| Segment {
                parts: seg.split('^').map(|p| p.to_string()).collect(),
            })
            .collect();
        UrlFilter {
            anchor_start,
            domain_anchor,
            anchor_end,
            segments,
        }
    }

    pub fn matches(&self, url: &str) -> bool {
        let hay = url.to_ascii_lowercase();
        let bytes = hay.as_bytes();

        // Determine the starting cursor and a constraint on where the first
        // segment may begin.
        // For domain_anchor we must begin at the start of a (sub-)domain in
        // the authority. We find candidate domain-start offsets and try each.
        if self.domain_anchor {
            for start in domain_anchor_offsets(&hay) {
                if self.match_from(bytes, start, true) {
                    return true;
                }
            }
            return false;
        }

        // anchor_start: first segment must match at offset 0.
        if self.anchor_start {
            return self.match_from(bytes, 0, true);
        }

        // Unanchored: the matcher's first segment can begin anywhere; the
        // greedy `match_segments` handles that via `find`.
        self.match_from(bytes, 0, false)
    }

    /// Try to match all segments starting at/after `start`. When `anchored`
    /// is true the first segment must begin exactly at `start`.
    fn match_from(&self, hay: &[u8], start: usize, anchored: bool) -> bool {
        let segs = &self.segments;
        // A leading empty segment (filter began with `*`) means "match
        // anywhere after"; treat as not-anchored for the first real chunk.
        match_segments(hay, start, segs, anchored, self.anchor_end)
    }
}

/// The set of byte offsets in `url` where a (sub-)domain begins, used for
/// the `||` domain anchor. That's the offset just after `://`, plus the
/// offset after each `.` within the host.
fn domain_anchor_offsets(url: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let bytes = url.as_bytes();
    let Some(scheme_pos) = url.find("://") else {
        // No scheme; allow domain anchor at byte 0.
        out.push(0);
        return out;
    };
    let host_start = scheme_pos + 3;
    out.push(host_start);
    // Walk the authority (until `/`, `?`, `#`) and record offsets after `.`.
    let mut i = host_start;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'/' || c == b'?' || c == b'#' {
            break;
        }
        if c == b'.' && i + 1 < bytes.len() {
            out.push(i + 1);
        }
        i += 1;
    }
    out
}

/// Match `segments` (split on `*`) against `hay` starting at `start`.
/// `anchored` requires segment 0 to begin exactly at `start`. `anchor_end`
/// requires the final segment to end at the end of `hay`.
fn match_segments(
    hay: &[u8],
    start: usize,
    segments: &[Segment],
    anchored: bool,
    anchor_end: bool,
) -> bool {
    let mut cursor = start;
    let n = segments.len();
    for (i, seg) in segments.iter().enumerate() {
        let first = i == 0;
        let last = i == n - 1;
        let anchor_this_start = first && anchored;
        let anchor_this_end = last && anchor_end;
        match match_one_segment(hay, cursor, seg, anchor_this_start, anchor_this_end) {
            Some(next) => cursor = next,
            None => return false,
        }
    }
    true
}

/// Match a single segment (a sequence of literal `parts` separated by `^`
/// separators) within `hay` starting search at `from`. Returns the cursor
/// just past the matched segment on success.
fn match_one_segment(
    hay: &[u8],
    from: usize,
    seg: &Segment,
    anchor_start: bool,
    anchor_end: bool,
) -> Option<usize> {
    // An empty segment (e.g. from leading/trailing `*` or `**`) matches the
    // empty string at `from`.
    if seg.parts.len() == 1 && seg.parts[0].is_empty() {
        if anchor_end && from != hay.len() {
            // empty trailing segment with end-anchor: only matches at end
            return if from == hay.len() { Some(from) } else { None };
        }
        return Some(from);
    }

    // Find a place where the whole segment matches. If anchored at start, the
    // only candidate is `from`; otherwise scan forward.
    let mut candidate = from;
    loop {
        if let Some(end) = try_match_segment_at(hay, candidate, seg, anchor_end) {
            return Some(end);
        }
        if anchor_start {
            return None;
        }
        if candidate >= hay.len() {
            return None;
        }
        candidate += 1;
    }
}

/// Try to match `seg` exactly at offset `pos`. `^` between parts requires a
/// separator char (non [A-Za-z0-9_-.%]) or end-of-URL.
fn try_match_segment_at(hay: &[u8], pos: usize, seg: &Segment, anchor_end: bool) -> Option<usize> {
    let mut cur = pos;
    let np = seg.parts.len();
    for (i, part) in seg.parts.iter().enumerate() {
        let pb = part.as_bytes();
        if cur + pb.len() > hay.len() {
            return None;
        }
        if &hay[cur..cur + pb.len()] != pb {
            return None;
        }
        cur += pb.len();
        // Between parts (and there were `^` separators), require a separator
        // char or end-of-URL — except after the very last part.
        if i + 1 < np {
            if cur == hay.len() {
                // `^` matches end-of-URL.
            } else if is_separator(hay[cur]) {
                cur += 1;
            } else {
                return None;
            }
        }
    }
    if anchor_end && cur != hay.len() {
        return None;
    }
    Some(cur)
}

/// DNR `^` separator: anything that is not a letter, digit, `_`, `-`, `.`,
/// or `%`. (End-of-URL is handled by callers.)
fn is_separator(b: u8) -> bool {
    !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'%'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn uf(s: &str) -> UrlFilter {
        UrlFilter::parse(s)
    }

    #[test]
    fn url_filter_substring() {
        assert!(uf("abc").matches("https://abcd.com/"));
        assert!(uf("abc").matches("https://example.com/abcd"));
        assert!(!uf("abc").matches("https://ab.com/"));
    }

    #[test]
    fn url_filter_wildcard() {
        assert!(uf("abc*d").matches("https://abcd.com/"));
        assert!(uf("abc*d").matches("https://example.com/abcxyzd"));
        assert!(!uf("abc*d").matches("https://abc.com/"));
    }

    #[test]
    fn url_filter_domain_anchor() {
        let f = uf("||a.example.com");
        assert!(f.matches("https://a.example.com/"));
        assert!(f.matches("https://b.a.example.com/xyz"));
        // domain anchor matches the registrable-ish boundary; "a.example.com"
        // as a leading substring of the host also qualifies.
        assert!(f.matches("https://a.example.company/"));
        assert!(!f.matches("https://example.com/"));
    }

    #[test]
    fn url_filter_domain_anchor_wsj() {
        // The actual BPC WSJ rule shape.
        let f = uf("||wsj.com");
        assert!(f.matches(
            "https://www.wsj.com/tech/ai/meta-keeps-delaying-the-release-f8569c8c"
        ));
        assert!(f.matches("https://wsj.com/"));
        assert!(!f.matches("https://notwsj.com/"));
        assert!(!f.matches("https://example.com/?ref=wsj.com"));
    }

    #[test]
    fn url_filter_left_anchor() {
        assert!(uf("|https*").matches("https://example.com/"));
        assert!(!uf("|https*").matches("http://example.com/"));
    }

    #[test]
    fn url_filter_separator_and_end_anchor() {
        // "example*^123|" — from the official docs table.
        let f = uf("example*^123|");
        assert!(f.matches("https://example.com/123"));
        assert!(f.matches("http://abc.com/example?123"));
        assert!(!f.matches("https://example.com/1234"));
        assert!(!f.matches("https://abc.com/example0123"));
    }

    #[test]
    fn case_insensitive() {
        assert!(uf("||WSJ.com").matches("https://www.wsj.com/x"));
    }

    #[test]
    fn parse_referer_rule_from_json() {
        let v = json!({
            "id": 5,
            "priority": 1,
            "action": {
                "type": "modifyHeaders",
                "requestHeaders": [
                    {"header": "Referer", "operation": "set", "value": "https://www.drudgereport.com/"}
                ]
            },
            "condition": {
                "urlFilter": "||wsj.com",
                "resourceTypes": ["main_frame", "sub_frame", "xmlhttprequest", "script"]
            }
        });
        let rule = HeaderRule::from_json(&v).expect("should parse");
        assert_eq!(rule.id, 5);
        assert_eq!(rule.request_headers.len(), 1);
        let op = &rule.request_headers[0];
        assert_eq!(op.header, "referer");
        assert_eq!(op.operation, HeaderOperation::Set);
        assert_eq!(op.value.as_deref(), Some("https://www.drudgereport.com/"));
        assert!(rule.matches(
            "https://www.wsj.com/tech/ai/meta-keeps-delaying-f8569c8c",
            "main_frame"
        ));
        // A non-matching host.
        assert!(!rule.matches("https://www.nytimes.com/x", "main_frame"));
    }

    #[test]
    fn non_modify_headers_rule_is_ignored() {
        let v = json!({
            "id": 1, "priority": 1,
            "action": {"type": "block"},
            "condition": {"urlFilter": "ads"}
        });
        assert!(HeaderRule::from_json(&v).is_none());
    }

    #[test]
    fn modify_headers_without_request_headers_is_ignored() {
        // Only responseHeaders -> nothing for us to do on the request.
        let v = json!({
            "id": 1, "priority": 1,
            "action": {
                "type": "modifyHeaders",
                "responseHeaders": [{"header": "x", "operation": "remove"}]
            },
            "condition": {"urlFilter": "x"}
        });
        assert!(HeaderRule::from_json(&v).is_none());
    }

    #[test]
    fn resource_type_filter_excludes_mismatch() {
        let v = json!({
            "id": 1, "priority": 1,
            "action": {"type": "modifyHeaders", "requestHeaders": [
                {"header": "Cookie", "operation": "set", "value": ""}
            ]},
            "condition": {"urlFilter": "||wsj.com", "resourceTypes": ["stylesheet", "image"]}
        });
        let rule = HeaderRule::from_json(&v).unwrap();
        assert!(!rule.matches("https://www.wsj.com/article", "main_frame"));
        assert!(rule.matches("https://www.wsj.com/a.css", "stylesheet"));
    }

    #[test]
    fn request_domains_gate() {
        let v = json!({
            "id": 1, "priority": 1,
            "action": {"type": "modifyHeaders", "requestHeaders": [
                {"header": "Referer", "operation": "set", "value": "https://g/"}
            ]},
            "condition": {"requestDomains": ["wsj.com"]}
        });
        let rule = HeaderRule::from_json(&v).unwrap();
        assert!(rule.matches("https://www.wsj.com/x", "main_frame"));
        assert!(!rule.matches("https://example.com/x", "main_frame"));
    }

    #[test]
    fn regex_filter() {
        let v = json!({
            "id": 1, "priority": 1,
            "action": {"type": "modifyHeaders", "requestHeaders": [
                {"header": "Referer", "operation": "set", "value": "https://g/"}
            ]},
            "condition": {"regexFilter": "^https://www\\.wsj\\.com/"}
        });
        let rule = HeaderRule::from_json(&v).unwrap();
        assert!(rule.matches("https://www.wsj.com/tech/x", "main_frame"));
        assert!(!rule.matches("https://www.wsj.com.evil.com/", "main_frame"));
    }
}
