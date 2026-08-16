use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{
    Client, Response,
    header::{ACCEPT, CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use url::Url;

use crate::{
    cache::{KeyedLocks, TimedCache},
    network,
};

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const FOUND_TTL: Duration = Duration::from_hours(6);
const MISSING_TTL: Duration = Duration::from_mins(15);
const MAX_IMAGE_BYTES: usize = 512 * 1024;
const MAX_HTML_BYTES: usize = 512 * 1024;
const MAX_REDIRECTS: usize = 3;
pub const FOUND_CACHE_CONTROL: &str = "public, max-age=21600, stale-if-error=86400";
pub const MISSING_CACHE_CONTROL: &str = "public, max-age=900";

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

pub struct Service {
    client: Client,
    cache: TimedCache<(), CachedValue>,
    locks: KeyedLocks,
}

impl Service {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(Policy::none())
                .timeout(FETCH_TIMEOUT)
                .user_agent(network::USER_AGENT)
                .build()?,
            cache: TimedCache::default(),
            locks: KeyedLocks::default(),
        })
    }

    pub fn clear(&self) {
        self.cache.clear();
    }

    pub async fn get(&self, page_url: &Url, favicon_url: Option<&Url>) -> Option<Favicon> {
        let page_origin = page_url.origin().ascii_serialization();
        let key = favicon_url.map_or_else(|| page_origin.clone(), |url| url.as_str().to_owned());
        if let Some(value) = self.cached(&key, true) {
            return found(value);
        }

        let lock = self.locks.get(&key);
        let _guard = lock.lock().await;
        if let Some(value) = self.cached(&key, true) {
            return found(value);
        }

        let stale = self.cached(&key, false);
        let value = match fetch_favicon(&self.client, page_url, favicon_url).await {
            Ok(icon) => CachedValue::Found(icon),
            Err(error) => {
                if let Some(CachedValue::Found(icon)) = stale {
                    tracing::warn!(origin = %page_origin, %error, "favicon refresh failed; using stale icon");
                    CachedValue::Found(icon)
                } else {
                    tracing::debug!(origin = %page_origin, %error, "favicon unavailable");
                    CachedValue::Missing
                }
            }
        };
        self.cache.insert(key, (), value.clone());
        found(value)
    }

    fn cached(&self, key: &str, require_fresh: bool) -> Option<CachedValue> {
        let cached = self.cache.get_with_ttl(key, &(), |value| match value {
            CachedValue::Found(_) => FOUND_TTL,
            CachedValue::Missing => MISSING_TTL,
        })?;
        (!require_fresh || cached.fresh).then_some(cached.value)
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
    if !network::same_origin(page_url, &icon_url) {
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
        if !network::same_origin(origin, &url) {
            bail!("favicon request left the configured origin");
        }
        let response = client
            .get(url.clone())
            .header(ACCEPT, accept)
            .send()
            .await?;
        if !network::is_redirect(response.status()) {
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
        if rel.split_ascii_whitespace().any(|value| {
            value.eq_ignore_ascii_case("icon")
                || value
                    .get(value.len().saturating_sub("-icon".len())..)
                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case("-icon"))
        }) && let Some(href) = attribute_value(tag, "href")
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
            find_icon_href("<LINK REL=APPLE-TOUCH-ICON HREF=/touch.png>"),
            Some("/touch.png".into())
        );
    }
}
