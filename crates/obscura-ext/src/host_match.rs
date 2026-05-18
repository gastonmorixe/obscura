//! WebExtension match-pattern matcher.
//!
//! Supports the subset commonly used by real-world content-script
//! extensions:
//! - `<scheme>://<host>/<path>`
//! - scheme: `*`, `http`, `https`, `file`, `ftp`
//! - host: `*`, `*.example.com`, or `example.com`
//! - path: `*` or literal with `*` wildcard
//!
//! Not implemented: `<all_urls>` (rarely requested in practice; can be
//! added when an extension that needs it shows up).

use url::Url;

/// A parsed match pattern. Cheap to test against many URLs.
#[derive(Debug, Clone)]
pub struct MatchPattern {
    raw: String,
    scheme: SchemeMatch,
    host: HostMatch,
    path_pattern: String, // path with `*` wildcards
}

#[derive(Debug, Clone)]
enum SchemeMatch {
    Any,
    Exact(String),
}

#[derive(Debug, Clone)]
enum HostMatch {
    /// `*` — any host.
    Any,
    /// `*.example.com` — example.com itself plus any subdomain.
    SuffixOrSelf(String),
    /// `example.com` — exact host only.
    Exact(String),
}

impl MatchPattern {
    pub fn parse(pat: &str) -> Option<Self> {
        // Split scheme.
        let (scheme_str, rest) = pat.split_once("://")?;
        let scheme = match scheme_str {
            "*" => SchemeMatch::Any,
            other => SchemeMatch::Exact(other.to_string()),
        };

        // Split host / path.
        let (host_str, path_str) = match rest.split_once('/') {
            Some((h, p)) => (h, format!("/{p}")),
            None => (rest, "/".to_string()),
        };

        let host = if host_str == "*" {
            HostMatch::Any
        } else if let Some(suffix) = host_str.strip_prefix("*.") {
            HostMatch::SuffixOrSelf(suffix.to_ascii_lowercase())
        } else {
            HostMatch::Exact(host_str.to_ascii_lowercase())
        };

        Some(Self {
            raw: pat.to_string(),
            scheme,
            host,
            path_pattern: path_str,
        })
    }

    pub fn matches_url(&self, url: &Url) -> bool {
        // Scheme.
        let url_scheme = url.scheme();
        match &self.scheme {
            SchemeMatch::Any => {
                if !matches!(url_scheme, "http" | "https" | "ftp" | "file") {
                    return false;
                }
            }
            SchemeMatch::Exact(s) => {
                if url_scheme != s {
                    return false;
                }
            }
        }

        // Host.
        let url_host = match url.host_str() {
            Some(h) => h.to_ascii_lowercase(),
            None => {
                // `file:` URLs have no host. Only allow if pattern is `*` host.
                return matches!(self.host, HostMatch::Any);
            }
        };
        match &self.host {
            HostMatch::Any => {}
            HostMatch::SuffixOrSelf(suffix) => {
                if url_host != *suffix && !url_host.ends_with(&format!(".{suffix}")) {
                    return false;
                }
            }
            HostMatch::Exact(h) => {
                if url_host != *h {
                    return false;
                }
            }
        }

        // Path. The url crate may give `/path` or `/path?query`. The
        // extension match pattern's path is matched against path WITHOUT
        // query/fragment per the WebExtensions spec, so we use url.path().
        let url_path = url.path();
        path_pattern_match(&self.path_pattern, url_path)
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }
}

/// Match a path pattern like `/news/*` against a concrete path `/news/foo`.
/// Wildcard `*` matches any (possibly empty) substring.
fn path_pattern_match(pattern: &str, target: &str) -> bool {
    // Fast paths.
    if pattern == "/*" || pattern == "*" {
        return true;
    }
    // Greedy splitter on `*`.
    let mut t = target;
    let parts: Vec<&str> = pattern.split('*').collect();
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !t.starts_with(part) {
                return false;
            }
            t = &t[part.len()..];
        } else if i == last {
            return t.ends_with(part);
        } else {
            match t.find(part) {
                Some(idx) => {
                    t = &t[idx + part.len()..];
                }
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pat: &str, url: &str) -> bool {
        MatchPattern::parse(pat)
            .unwrap()
            .matches_url(&Url::parse(url).unwrap())
    }

    #[test]
    fn star_host_matches_subdomain_and_root() {
        assert!(matches("*://*.example.com/*", "https://www.example.com/x"));
        assert!(matches(
            "*://*.example.com/*",
            "https://example.com/news/foo"
        ));
        assert!(!matches(
            "*://*.example.com/*",
            "https://example.com.evil.com/"
        ));
    }

    #[test]
    fn exact_scheme_matches_only_that() {
        assert!(matches("https://*.example.com/*", "https://x.example.com/a"));
        assert!(!matches("https://*.example.com/*", "http://x.example.com/a"));
    }

    #[test]
    fn star_scheme_excludes_data_uri() {
        let pat = MatchPattern::parse("*://*/*").unwrap();
        assert!(!pat.matches_url(&Url::parse("data:text/html,hello").unwrap()));
    }
}
