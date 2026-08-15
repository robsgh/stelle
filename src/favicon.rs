use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use reqwest::{
    Client, Response, StatusCode,
    header::{ACCEPT, CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use url::Url;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const FOUND_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const MISSING_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_IMAGE_BYTES: usize = 512 * 1024;
const MAX_HTML_BYTES: usize = 512 * 1024;
const MAX_REDIRECTS: usize = 3;

#[derive(Clone)]
pub struct Favicon {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

#[derive(Clone)]
enum CachedValue {
    Found(Favicon),
    Missing,
}

struct CachedFavicon {
    value: CachedValue,
    refreshed_at: Instant,
}

pub struct Service {
    client: Client,
    cache: RwLock<HashMap<String, CachedFavicon>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Service {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(Policy::none())
                .timeout(FETCH_TIMEOUT)
                .user_agent(concat!("stelle/", env!("CARGO_PKG_VERSION")))
                .build()?,
            cache: RwLock::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn clear(&self) {
        self.cache
            .write()
            .expect("favicon cache lock poisoned")
            .clear();
    }

    pub async fn get(&self, page_url: &Url, favicon_url: Option<&Url>) -> Option<Favicon> {
        let key = page_url.origin().ascii_serialization();
        if let Some(value) = self.cached(&key, true) {
            return found(value);
        }

        let lock = self
            .locks
            .lock()
            .expect("favicon lock map poisoned")
            .entry(key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        if let Some(value) = self.cached(&key, true) {
            return found(value);
        }

        let stale = self.cached(&key, false);
        let value = match fetch_favicon(&self.client, page_url, favicon_url).await {
            Ok(icon) => CachedValue::Found(icon),
            Err(error) => {
                if let Some(CachedValue::Found(icon)) = stale {
                    tracing::warn!(origin = %key, %error, "favicon refresh failed; using stale icon");
                    CachedValue::Found(icon)
                } else {
                    tracing::debug!(origin = %key, %error, "favicon unavailable");
                    CachedValue::Missing
                }
            }
        };
        self.cache
            .write()
            .expect("favicon cache lock poisoned")
            .insert(
                key,
                CachedFavicon {
                    value: value.clone(),
                    refreshed_at: Instant::now(),
                },
            );
        found(value)
    }

    fn cached(&self, key: &str, require_fresh: bool) -> Option<CachedValue> {
        let cache = self.cache.read().expect("favicon cache lock poisoned");
        let cached = cache.get(key)?;
        let ttl = match cached.value {
            CachedValue::Found(_) => FOUND_TTL,
            CachedValue::Missing => MISSING_TTL,
        };
        if require_fresh && cached.refreshed_at.elapsed() >= ttl {
            return None;
        }
        Some(cached.value.clone())
    }
}

fn found(value: CachedValue) -> Option<Favicon> {
    match value {
        CachedValue::Found(icon) => Some(icon),
        CachedValue::Missing => None,
    }
}

async fn fetch_favicon(
    client: &Client,
    page_url: &Url,
    favicon_url: Option<&Url>,
) -> Result<Favicon> {
    if let Some(favicon_url) = favicon_url {
        return fetch_image(client, favicon_url, favicon_url.clone()).await;
    }
    let root_icon = page_url.join("/favicon.ico")?;
    if let Ok(icon) = fetch_image(client, page_url, root_icon).await {
        return Ok(icon);
    }

    let (document_url, html) = fetch_html(client, page_url).await?;
    let href = find_icon_href(&html).context("page did not advertise an icon")?;
    let icon_url = document_url.join(&href)?;
    if !same_origin(page_url, &icon_url) {
        bail!("page icon used a different origin");
    }
    fetch_image(client, page_url, icon_url).await
}

async fn fetch_image(client: &Client, origin: &Url, url: Url) -> Result<Favicon> {
    let response = get_same_origin(
        client,
        origin,
        url,
        "image/avif,image/webp,image/*,*/*;q=0.1",
    )
    .await?;
    if !response.status().is_success() {
        bail!("favicon returned HTTP {}", response.status());
    }
    let bytes = read_limited(response, MAX_IMAGE_BYTES).await?;
    let content_type = image_content_type(&bytes).context("favicon was not a supported image")?;
    Ok(Favicon {
        bytes,
        content_type,
    })
}

async fn fetch_html(client: &Client, page_url: &Url) -> Result<(Url, String)> {
    let response = get_same_origin(client, page_url, page_url.clone(), "text/html").await?;
    if !response.status().is_success() {
        bail!("page returned HTTP {}", response.status());
    }
    if response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| !value.to_ascii_lowercase().starts_with("text/html"))
    {
        bail!("page did not return HTML");
    }
    let final_url = response.url().clone();
    let bytes = read_limited(response, MAX_HTML_BYTES).await?;
    Ok((final_url, String::from_utf8_lossy(&bytes).into_owned()))
}

async fn get_same_origin(
    client: &Client,
    origin: &Url,
    mut url: Url,
    accept: &'static str,
) -> Result<Response> {
    for redirect_count in 0..=MAX_REDIRECTS {
        if !same_origin(origin, &url) {
            bail!("favicon request left the configured origin");
        }
        let response = client
            .get(url.clone())
            .header(ACCEPT, accept)
            .send()
            .await?;
        if !is_redirect(response.status()) {
            return Ok(response);
        }
        if redirect_count == MAX_REDIRECTS {
            bail!("too many favicon redirects");
        }
        let location = response
            .headers()
            .get(LOCATION)
            .context("redirect did not include a location")?
            .to_str()?;
        url = url.join(location)?;
    }
    unreachable!()
}

async fn read_limited(mut response: Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        bail!("favicon response exceeded the size limit");
    }
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(limit as u64) as usize,
    );
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > limit {
            bail!("favicon response exceeded the size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

pub fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn image_content_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\0\0\x01\0") {
        Some("image/x-icon")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && (&bytes[8..12] == b"avif" || &bytes[8..12] == b"avis")
    {
        Some("image/avif")
    } else {
        None
    }
}

fn find_icon_href(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(relative_start) = lower[offset..].find("<link") {
        let start = offset + relative_start;
        let end = start + lower[start..].find('>')? + 1;
        let tag = &html[start..end];
        let rel = attribute_value(tag, "rel").unwrap_or_default();
        if rel
            .split_ascii_whitespace()
            .any(|value| value.eq_ignore_ascii_case("icon") || value.ends_with("-icon"))
            && let Some(href) = attribute_value(tag, "href")
        {
            return Some(href.replace("&amp;", "&"));
        }
        offset = end;
    }
    None
}

fn attribute_value(tag: &str, target: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut index = tag.find(char::is_whitespace).unwrap_or(tag.len());
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'/') {
            index += 1;
        }
        let name_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && !matches!(bytes[index], b'=' | b'>' | b'/')
        {
            index += 1;
        }
        if name_start == index {
            break;
        }
        let name = &tag[name_start..index];
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let (value_start, value_end) = if index < bytes.len()
            && matches!(bytes[index], b'\'' | b'"')
        {
            let quote = bytes[index];
            index += 1;
            let start = index;
            while index < bytes.len() && bytes[index] != quote {
                index += 1;
            }
            let end = index;
            index += usize::from(index < bytes.len());
            (start, end)
        } else {
            let start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() && bytes[index] != b'>'
            {
                index += 1;
            }
            (start, index)
        };
        if name.eq_ignore_ascii_case(target) {
            return Some(tag[value_start..value_end].to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_images_without_trusting_headers() {
        assert_eq!(image_content_type(b"\0\0\x01\0rest"), Some("image/x-icon"));
        assert_eq!(
            image_content_type(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(image_content_type(b"<html>not an icon</html>"), None);
    }

    #[test]
    fn discovers_quoted_and_unquoted_icon_links() {
        assert_eq!(
            find_icon_href(r#"<link sizes="32x32" rel="shortcut icon" href='/app.png'>"#),
            Some("/app.png".into())
        );
        assert_eq!(
            find_icon_href("<LINK REL=apple-touch-icon HREF=/touch.png>"),
            Some("/touch.png".into())
        );
    }

    #[test]
    fn origin_comparison_includes_scheme_and_port() {
        let origin = Url::parse("https://example.com:8443/page").unwrap();
        assert!(same_origin(
            &origin,
            &Url::parse("https://example.com:8443/favicon.ico").unwrap()
        ));
        assert!(!same_origin(
            &origin,
            &Url::parse("https://example.com/favicon.ico").unwrap()
        ));
    }
}
