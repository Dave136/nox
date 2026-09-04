//! Safe, same-origin favicon fetching for Login website values.
//!
//! Requests go directly to the saved site's HTTPS origin. Responses are bounded
//! and validated before callers put them in the device-local icon cache. This
//! module never writes an encrypted item payload.

use futures::AsyncReadExt;
use gpui::http_client::{AsyncBody, HttpClient, HttpRequestExt, RedirectPolicy, Request, Url};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

// `FakeHttpClient` ignores the request-builder timeout, so tests exercise the
// explicit future race below. Keep the test deadline short without changing
// the production bound.
#[cfg(not(test))]
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const FETCH_TIMEOUT: Duration = Duration::from_millis(50);
const MAX_ICON_BYTES: usize = 5 * 1024 * 1024;
const MAX_HTML_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum FaviconError {
    /// None of the saved website values could be parsed into a web origin.
    NoUsableUri,
    /// Every candidate returned a non-usable response.
    AllCandidatesFailed,
    /// A response claimed to be an image but failed image validation.
    InvalidImage,
    /// A linked icon or redirect points at another origin.
    CrossOrigin,
}

#[derive(Debug)]
struct FetchedBody {
    bytes: Vec<u8>,
    content_type: Option<String>,
}

/// Try website values in stored order. For each origin, `/favicon.ico` is tried
/// before the same-origin HTML link fallback. The first validated image wins.
pub(crate) async fn fetch_favicon(
    client: &dyn HttpClient,
    uris: &[String],
) -> Result<Vec<u8>, FaviconError> {
    let mut last_error = FaviconError::NoUsableUri;
    for uri in uris {
        let Some(origin) = extract_origin(uri) else {
            continue;
        };
        match fetch_favicon_for_origin(client, &origin).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

async fn fetch_favicon_for_origin(
    client: &dyn HttpClient,
    origin: &str,
) -> Result<Vec<u8>, FaviconError> {
    let favicon_url = format!("{origin}/favicon.ico");
    let favicon_error = match fetch_image(client, &favicon_url).await {
        Ok(bytes) => return Ok(bytes),
        Err(error) => error,
    };
    match fetch_html_linked_icon(client, origin).await {
        Ok(bytes) => Ok(bytes),
        Err(error) => {
            if matches!(
                error,
                FaviconError::CrossOrigin | FaviconError::InvalidImage
            ) {
                Err(error)
            } else {
                Err(favicon_error)
            }
        }
    }
}

/// Return the hostname used by the request/cache compatibility helpers.
/// Schemeless `host:port` values are interpreted as HTTPS authorities.
pub(crate) fn extract_host(uri: &str) -> Option<String> {
    parse_site_url(uri).and_then(|url| url.host_str().map(str::to_owned))
}

/// Return an HTTPS origin with a non-default port preserved. The caller uses
/// this for both the request URL and same-origin comparisons.
pub(crate) fn extract_origin(uri: &str) -> Option<String> {
    let url = parse_site_url(uri)?;
    let host = url.host_str()?;
    let authority = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let authority = match url.port() {
        Some(port) => format!("{authority}:{port}"),
        None => authority,
    };
    Some(format!("https://{authority}"))
}

fn parse_site_url(uri: &str) -> Option<Url> {
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    let parsed = Url::parse(uri)
        .ok()
        .filter(|url| is_web_url(url) && url.host_str().is_some())
        .or_else(|| {
            // No-scheme fallback is for human-entered hosts like example.com or
            // localhost:3000. Do not reinterpret opaque schemes such as
            // mailto:user@example.test as https://mailto:user@example.test.
            if uri
                .split_once(':')
                .is_some_and(|(prefix, suffix)| suffix.contains('@') && is_scheme_like(prefix))
            {
                return None;
            }
            Url::parse(&format!("https://{uri}"))
                .ok()
                .filter(|url| is_web_url(url) && url.host_str().is_some())
        })?;
    Some(parsed)
}

fn is_scheme_like(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn is_web_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

async fn fetch_html_linked_icon(
    client: &dyn HttpClient,
    origin: &str,
) -> Result<Vec<u8>, FaviconError> {
    let root = fetch_capped(client, &format!("{origin}/"), MAX_HTML_BYTES).await?;
    let html = std::str::from_utf8(&root.bytes).map_err(|_| FaviconError::AllCandidatesFailed)?;
    let href = find_icon_link(html).ok_or(FaviconError::AllCandidatesFailed)?;
    let base = Url::parse(&format!("{origin}/")).map_err(|_| FaviconError::AllCandidatesFailed)?;
    let icon_url = base.join(&href).map_err(|_| FaviconError::CrossOrigin)?;
    if !same_origin(&base, &icon_url) || icon_url.scheme() != "https" {
        return Err(FaviconError::CrossOrigin);
    }
    fetch_image(client, icon_url.as_str()).await
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

/// Scan bounded, untrusted HTML for the first icon link. Attribute order and
/// whitespace are intentionally accepted, while the parser remains small and
/// does not execute or interpret any other HTML.
fn find_icon_link(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative_start) = lower[cursor..].find("<link") {
        let start = cursor + relative_start;
        let end = lower[start..].find('>')? + start;
        let tag = &html[start..=end];
        let lower_tag = &lower[start..=end];
        let rel = extract_attribute(lower_tag, tag, "rel")?;
        let rel_tokens = rel.split_ascii_whitespace().collect::<Vec<_>>();
        let is_icon = rel_tokens.contains(&"icon")
            || (rel_tokens.contains(&"shortcut") && rel_tokens.contains(&"icon"));
        if is_icon && let Some(href) = extract_attribute(lower_tag, tag, "href") {
            return Some(href);
        }
        cursor = end + 1;
    }
    None
}

fn extract_attribute(lower_tag: &str, original_tag: &str, name: &str) -> Option<String> {
    let mut cursor = 0;
    while let Some(relative) = lower_tag[cursor..].find(name) {
        let start = cursor + relative;
        let before = lower_tag.as_bytes().get(start.wrapping_sub(1)).copied();
        let after = lower_tag.as_bytes().get(start + name.len()).copied();
        if before.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || after.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            cursor = start + name.len();
            continue;
        }
        let mut equals = start + name.len();
        while lower_tag
            .as_bytes()
            .get(equals)
            .is_some_and(u8::is_ascii_whitespace)
        {
            equals += 1;
        }
        if lower_tag.as_bytes().get(equals) != Some(&b'=') {
            cursor = start + name.len();
            continue;
        }
        equals += 1;
        while lower_tag
            .as_bytes()
            .get(equals)
            .is_some_and(u8::is_ascii_whitespace)
        {
            equals += 1;
        }
        let quote = *original_tag.as_bytes().get(equals)?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let value_start = equals + 1;
        let value_end = original_tag[value_start..]
            .find(char::from(quote))?
            .checked_add(value_start)?;
        return Some(original_tag[value_start..value_end].to_owned());
    }
    None
}

async fn fetch_image(client: &dyn HttpClient, url: &str) -> Result<Vec<u8>, FaviconError> {
    let response = fetch_capped(client, url, MAX_ICON_BYTES).await?;
    let Some(content_type) = response.content_type.as_deref() else {
        return Err(FaviconError::InvalidImage);
    };
    if !crate::icons::is_image_content_type(content_type) {
        return Err(FaviconError::InvalidImage);
    }
    crate::icons::validate_local_image(&response.bytes).map_err(|_| FaviconError::InvalidImage)?;
    Ok(response.bytes)
}

/// GET a bounded response. Redirects are explicitly disabled; a 3xx response
/// is rejected before its body is read, so a third-party redirect is never
/// followed or cached.
async fn fetch_capped(
    client: &dyn HttpClient,
    url: &str,
    max_bytes: usize,
) -> Result<FetchedBody, FaviconError> {
    let request = Request::builder()
        .uri(url)
        .timeout(FETCH_TIMEOUT)
        .follow_redirects(RedirectPolicy::NoFollow)
        .body(AsyncBody::empty())
        .map_err(|_| FaviconError::AllCandidatesFailed)?;
    let mut response = with_timeout(client.send(request)).await?;
    if !response.status().is_success() {
        return Err(FaviconError::AllCandidatesFailed);
    }
    if response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(FaviconError::AllCandidatesFailed);
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut body = Vec::new();
    with_timeout(
        response
            .body_mut()
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut body),
    )
    .await?;
    if body.len() > max_bytes {
        return Err(FaviconError::AllCandidatesFailed);
    }
    Ok(FetchedBody {
        bytes: body,
        content_type,
    })
}

/// Bound both transport and response-body operations; a response can arrive
/// while its body remains stalled.
async fn with_timeout<T, E>(
    future: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, FaviconError> {
    let (deadline_tx, deadline_rx) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(FETCH_TIMEOUT);
        let _ = deadline_tx.send(());
    });
    match futures::future::select(Box::pin(future), deadline_rx).await {
        futures::future::Either::Left((result, _)) => {
            result.map_err(|_| FaviconError::AllCandidatesFailed)
        }
        futures::future::Either::Right(_) => Err(FaviconError::AllCandidatesFailed),
    }
}

/// Compatibility cache for older callers/tests. Feature code uses
/// `cache_favicon_for_key`, which is content-addressed by local editor/item key.
pub(crate) fn favicon_cache_path(data_dir: &Path, host: &str) -> PathBuf {
    let digest = Sha256::digest(host.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    data_dir.join("favicon-cache").join(hex)
}

pub(crate) fn cache_favicon(data_dir: &Path, host: &str, bytes: &[u8]) -> io::Result<()> {
    crate::icons::validate_local_image(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid image"))?;
    let path = favicon_cache_path(data_dir, host);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    std::fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    if path.is_file() {
        return Ok(());
    }
    let temp = parent.join(format!(".tmp-{}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    match std::fs::rename(temp, path) {
        Ok(()) => Ok(()),
        Err(_) if favicon_cache_path(data_dir, host).is_file() => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn cache_favicon_for_key(
    data_dir: &Path,
    local_key: &str,
    bytes: &[u8],
) -> Result<crate::icons::LocalIconRef, crate::icons::LocalIconError> {
    crate::icons::cache_local_icon(data_dir, local_key, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::http_client::{FakeHttpClient, Response};

    fn png_bytes() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0, 1, 0, 0, 0, 1,
        ]
    }

    fn image_response(bytes: Vec<u8>) -> Response<AsyncBody> {
        Response::builder()
            .status(200)
            .header("content-type", "image/png")
            .body(bytes.into())
            .unwrap()
    }

    #[gpui::test]
    async fn favicon_ico_success_skips_the_html_fallback() {
        let client = FakeHttpClient::create(move |request| {
            let bytes = png_bytes();
            async move {
                assert_eq!(request.uri().path(), "/favicon.ico");
                Ok(image_response(bytes))
            }
        });
        let result = fetch_favicon(client.as_ref(), &["https://example.test".to_owned()]).await;
        assert_eq!(result, Ok(png_bytes()));
    }

    #[gpui::test]
    async fn favicon_ico_404_falls_back_to_same_origin_html_icon() {
        let client = FakeHttpClient::create(move |request| {
            let bytes = png_bytes();
            async move {
                match request.uri().path() {
                    "/favicon.ico" => Ok(Response::builder()
                        .status(404)
                        .body(Vec::new().into())
                        .unwrap()),
                    "/" => Ok(Response::builder()
                        .status(200)
                        .header("content-type", "text/html")
                        .body(r#"<link href='/assets/icon.png' rel='shortcut icon'>"#.into())
                        .unwrap()),
                    "/assets/icon.png" => Ok(image_response(bytes)),
                    _ => Ok(Response::builder()
                        .status(404)
                        .body(Vec::new().into())
                        .unwrap()),
                }
            }
        });
        let result = fetch_favicon(client.as_ref(), &["https://example.test".to_owned()]).await;
        assert_eq!(result, Ok(png_bytes()));
    }

    #[gpui::test]
    async fn cross_origin_html_link_is_rejected_without_requesting_the_target() {
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests_for_client = requests.clone();
        let client = FakeHttpClient::create(move |request| {
            requests_for_client.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async move {
                match request.uri().path() {
                    "/favicon.ico" => Ok(Response::builder()
                        .status(404)
                        .body(Vec::new().into())
                        .unwrap()),
                    "/" => Ok(Response::builder()
                        .status(200)
                        .header("content-type", "text/html")
                        .body(r#"<link rel='icon' href='https://cdn.example/icon.png'>"#.into())
                        .unwrap()),
                    _ => panic!("cross-origin icon must not be requested"),
                }
            }
        });
        assert_eq!(
            fetch_favicon(client.as_ref(), &["https://example.test".to_owned()]).await,
            Err(FaviconError::CrossOrigin)
        );
        assert_eq!(requests.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[gpui::test]
    async fn html_text_and_malformed_image_responses_are_rejected() {
        let client = FakeHttpClient::create(move |request| async move {
            if request.uri().path() == "/favicon.ico" {
                Ok(Response::builder()
                    .status(200)
                    .header("content-type", "image/png")
                    .body(b"not an image".to_vec().into())
                    .unwrap())
            } else if request.uri().path() == "/" {
                Ok(Response::builder()
                    .status(200)
                    .header("content-type", "text/html")
                    .body(b"<html>soft 404</html>".to_vec().into())
                    .unwrap())
            } else {
                panic!("the malformed image must stop before another target")
            }
        });
        assert_eq!(
            fetch_favicon(client.as_ref(), &["https://example.test".to_owned()]).await,
            Err(FaviconError::InvalidImage)
        );
    }

    #[test]
    fn extract_host_handles_schemeless_host_port_without_dropping_the_port_origin() {
        assert_eq!(extract_host("nas.local:5000"), Some("nas.local".to_owned()));
        assert_eq!(extract_host("localhost:3000"), Some("localhost".to_owned()));
        assert_eq!(
            extract_origin("nas.local:5000"),
            Some("https://nas.local:5000".to_owned())
        );
        assert_eq!(
            extract_origin("https://example.test:8443/path"),
            Some("https://example.test:8443".to_owned())
        );
    }

    #[test]
    fn invalid_uri_has_no_origin() {
        assert_eq!(extract_origin("not a URI"), None);
        assert_eq!(extract_origin("mailto:user@example.test"), None);
    }

    #[test]
    fn a_stalled_response_body_times_out_instead_of_blocking_forever() {
        futures::executor::block_on(async {
            struct PendingReader;

            impl futures::AsyncRead for PendingReader {
                fn poll_read(
                    self: std::pin::Pin<&mut Self>,
                    _cx: &mut std::task::Context<'_>,
                    _buf: &mut [u8],
                ) -> std::task::Poll<std::io::Result<usize>> {
                    std::task::Poll::Pending
                }
            }

            let client = FakeHttpClient::create(|_request| async move {
                Ok(Response::builder()
                    .status(200)
                    .body(AsyncBody::from_reader(PendingReader))
                    .unwrap())
            });
            let uris = ["https://example.test".to_owned()];
            let fetch = fetch_favicon(client.as_ref(), &uris);
            let (deadline_tx, deadline_rx) = futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                std::thread::sleep(FETCH_TIMEOUT * 4);
                let _ = deadline_tx.send(());
            });

            let result = futures::future::select(Box::pin(fetch), deadline_rx).await;
            assert!(matches!(
                result,
                futures::future::Either::Left((Err(FaviconError::AllCandidatesFailed), _))
            ));
        });
    }

    #[test]
    fn cache_favicon_uses_restrictive_permissions_and_rejects_invalid_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "nox-favicon-cache-{}-{}",
            std::process::id(),
            crate::icons::MAX_LOCAL_IMAGE_BYTES
        ));
        assert!(cache_favicon(&dir, "example.test", &png_bytes()).is_ok());
        assert_eq!(
            std::fs::read(favicon_cache_path(&dir, "example.test")).unwrap(),
            png_bytes()
        );
        assert!(cache_favicon(&dir, "invalid.test", b"html").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(favicon_cache_path(&dir, "example.test"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
