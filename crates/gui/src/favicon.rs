//! Fetches a login item's site favicon on explicit user request, and caches
//! it to a local, unencrypted, per-host file. The favicon bytes never enter
//! the encrypted vault — only the user's choice to use one does
//! (`nox_core::IconChoice::Favicon`).
//!
//! Synchronous by design: called from inside `cx.background_executor()`
//! (same thread pool `Vault::create`/`unlock` already block on), not GPUI's
//! foreground executor, so a blocking `ureq` call is the simplest correct
//! thing here — no extra async plumbing to fetch one small file.

// ponytail: this module has no caller yet (Task 5/6 of the custom-item-icon
// plan wire the resolver and picker to it) — drop this allow once they do.
#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ICON_BYTES: u64 = 5 * 1024 * 1024;
const MAX_HTML_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum FaviconError {
    /// None of the item's saved URIs could be parsed into a usable host.
    NoUsableUri,
    /// Every candidate URI was tried and none produced a usable icon.
    AllCandidatesFailed,
}

/// Try each `uri` in order; for each, `favicon.ico` first, then the HTML
/// `<link rel="icon">` fallback. The first successful fetch wins — later
/// URIs are never tried once one succeeds. Always fetches over `https`,
/// regardless of the scheme (if any) the saved URI used.
pub(crate) fn fetch_favicon(uris: &[String]) -> Result<Vec<u8>, FaviconError> {
    let mut last_error = FaviconError::NoUsableUri;
    for uri in uris {
        let Some(host) = extract_host(uri) else {
            continue;
        };
        match fetch_from_base(&format!("https://{host}")) {
            Ok(bytes) => return Ok(bytes),
            Err(err) => last_error = err,
        }
    }
    Err(last_error)
}

pub(crate) fn extract_host(uri: &str) -> Option<String> {
    let with_scheme = if uri.contains("://") {
        uri.to_owned()
    } else {
        format!("https://{uri}")
    };
    let after_scheme = with_scheme.split("://").nth(1)?;
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()?
        .split('@')
        .next_back()?
        .split(':')
        .next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_owned())
    }
}

/// `favicon.ico` first, then the HTML `<link rel="icon">` fallback, against
/// `base` (e.g. `https://example.test`, or `http://127.0.0.1:PORT` in
/// tests) — the part that's actually testable without a real network.
fn fetch_from_base(base: &str) -> Result<Vec<u8>, FaviconError> {
    if let Ok(bytes) = fetch_capped(&format!("{base}/favicon.ico"), MAX_ICON_BYTES) {
        return Ok(bytes);
    }
    let html_bytes = fetch_capped(&format!("{base}/"), MAX_HTML_BYTES)
        .map_err(|_| FaviconError::AllCandidatesFailed)?;
    let html = String::from_utf8_lossy(&html_bytes);
    let href = find_icon_link(&html).ok_or(FaviconError::AllCandidatesFailed)?;
    let icon_url = resolve_icon_url(base, &href);
    fetch_capped(&icon_url, MAX_ICON_BYTES)
}

/// Case-insensitive scan for the first `<link rel="icon" ...>` or
/// `<link rel="shortcut icon" ...>` tag's `href`. Deliberately not a full
/// HTML parser — a plain substring/attribute scan over already-capped,
/// already-untrusted input is enough for this one attribute.
fn find_icon_link(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel_pos) = lower[search_from..].find("rel=") {
        let abs = search_from + rel_pos;
        let after_rel = &lower[abs + 4..];
        let is_icon_rel = after_rel.starts_with("\"icon\"")
            || after_rel.starts_with("'icon'")
            || after_rel.starts_with("\"shortcut icon\"")
            || after_rel.starts_with("'shortcut icon'");
        if is_icon_rel {
            let tag_start = lower[..abs].rfind("<link").unwrap_or(0);
            let tag_end = lower[abs..]
                .find('>')
                .map(|i| abs + i)
                .unwrap_or(lower.len());
            let tag = &html[tag_start..tag_end.min(html.len())];
            if let Some(href) = extract_href(tag) {
                return Some(href);
            }
        }
        search_from = abs + 4;
    }
    None
}

fn extract_href(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pos = lower.find("href=")?;
    let rest = &tag[pos + 5..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let end = rest[1..].find(quote)?;
    Some(rest[1..1 + end].to_owned())
}

fn resolve_icon_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        href.to_owned()
    } else if let Some(stripped) = href.strip_prefix('/') {
        format!("{base}/{stripped}")
    } else {
        format!("{base}/{href}")
    }
}

/// GET `url`, rejecting anything over `max_bytes` before it's fully read —
/// never allocate unbounded on an untrusted remote response.
fn fetch_capped(url: &str, max_bytes: u64) -> Result<Vec<u8>, FaviconError> {
    let response = ureq::get(url)
        .timeout(FETCH_TIMEOUT)
        .call()
        .map_err(|_| FaviconError::AllCandidatesFailed)?;
    let mut body = Vec::new();
    response
        .into_reader()
        .take(max_bytes + 1)
        .read_to_end(&mut body)
        .map_err(|_| FaviconError::AllCandidatesFailed)?;
    if body.len() as u64 > max_bytes {
        return Err(FaviconError::AllCandidatesFailed);
    }
    Ok(body)
}

/// `<data_dir>/favicon-cache/<hex(sha256(host))>` — one unencrypted file per
/// host, no extension (the renderer sniffs the image format from content,
/// the same way GPUI's own `img()` element already does).
pub(crate) fn favicon_cache_path(data_dir: &Path, host: &str) -> PathBuf {
    let digest = Sha256::digest(host.as_bytes());
    let hex = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    data_dir.join("favicon-cache").join(hex)
}

pub(crate) fn cache_favicon(data_dir: &Path, host: &str, bytes: &[u8]) -> std::io::Result<()> {
    let path = favicon_cache_path(data_dir, host);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::Arc;

    /// A minimal local HTTP/1.1 server for one test: `routes` maps a path
    /// (e.g. "/favicon.ico") to a (status, body) response. Serves requests
    /// on a background thread until the returned listener is dropped —
    /// no live network, same loopback-testing spirit `sync`'s own tests
    /// already use.
    fn spawn_test_server(routes: HashMap<&'static str, (u16, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let routes = Arc::new(routes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let Ok(n) = stream.read(&mut buf) else {
                    continue;
                };
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let (status, body) = routes
                    .get(path.as_str())
                    .cloned()
                    .unwrap_or((404, Vec::new()));
                let reason = if status == 200 { "OK" } else { "Not Found" };
                let header = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    fn favicon_bytes() -> Vec<u8> {
        vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4]
    }

    #[test]
    fn favicon_ico_success_skips_the_html_fallback() {
        let mut routes = HashMap::new();
        routes.insert("/favicon.ico", (200, favicon_bytes()));
        let base = spawn_test_server(routes);
        assert_eq!(fetch_from_base(&base).unwrap(), favicon_bytes());
    }

    #[test]
    fn favicon_ico_404_falls_back_to_the_html_linked_icon() {
        let mut routes = HashMap::new();
        routes.insert("/favicon.ico", (404, Vec::new()));
        routes.insert(
            "/",
            (
                200,
                br#"<html><head><link rel="icon" href="/static/icon.png"></head></html>"#.to_vec(),
            ),
        );
        routes.insert("/static/icon.png", (200, favicon_bytes()));
        let base = spawn_test_server(routes);
        assert_eq!(fetch_from_base(&base).unwrap(), favicon_bytes());
    }

    #[test]
    fn every_candidate_failing_is_an_error() {
        let routes = HashMap::new();
        let base = spawn_test_server(routes);
        assert_eq!(
            fetch_from_base(&base),
            Err(FaviconError::AllCandidatesFailed)
        );
    }

    #[test]
    fn oversized_response_is_rejected_before_being_fully_read() {
        let mut routes = HashMap::new();
        routes.insert(
            "/favicon.ico",
            (200, vec![0u8; (MAX_ICON_BYTES + 1) as usize]),
        );
        let base = spawn_test_server(routes);
        assert_eq!(
            fetch_from_base(&base),
            Err(FaviconError::AllCandidatesFailed)
        );
    }

    #[test]
    fn extract_host_accepts_a_bare_domain_without_a_scheme() {
        assert_eq!(
            extract_host("example.test"),
            Some("example.test".to_owned())
        );
        assert_eq!(
            extract_host("https://example.test/path"),
            Some("example.test".to_owned())
        );
        assert_eq!(
            extract_host("https://example.test:8443/path"),
            Some("example.test".to_owned())
        );
    }

    #[test]
    fn cache_path_is_stable_and_write_then_read_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("nox-favicon-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path_a = favicon_cache_path(&dir, "example.test");
        let path_b = favicon_cache_path(&dir, "example.test");
        assert_eq!(path_a, path_b);

        cache_favicon(&dir, "example.test", b"icon-bytes").unwrap();
        assert_eq!(std::fs::read(&path_a).unwrap(), b"icon-bytes");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
