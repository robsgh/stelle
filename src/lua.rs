use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use mlua::{Lua, LuaSerdeExt, Table, Value, VmState};
use reqwest::{
    StatusCode,
    blocking::Client,
    header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderName, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use url::Url;

use crate::config::{LuaWidget, validate_http_url};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MEMORY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const INTERRUPT_LIMIT: u64 = 250_000;
const MAX_CONCURRENT_WIDGETS: usize = 4;
pub const TIME_LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StatsContent {
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub href: Option<String>,
    pub metrics: Vec<Metric>,
    pub fetched_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    pub label: String,
    pub value: JsonValue,
}

#[derive(Clone)]
pub struct LuaRuntime {
    permits: Arc<tokio::sync::Semaphore>,
}

impl LuaRuntime {
    pub fn new() -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_WIDGETS)),
        }
    }

    pub async fn execute(&self, widget: &LuaWidget) -> Result<StatsContent> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .context("widget executor is unavailable")?;
        let widget = widget.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            Self::execute_blocking(&widget)
        })
        .await
        .context("widget executor stopped unexpectedly")?
    }

    fn execute_blocking(widget: &LuaWidget) -> Result<StatsContent> {
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(TIME_LIMIT)
            .user_agent(concat!("stelle/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let lua = Lua::new();
        lua.set_memory_limit(MEMORY_LIMIT_BYTES)?;

        let interrupts = Arc::new(AtomicU64::new(0));
        let started = Instant::now();
        lua.set_interrupt(move |_| {
            if interrupts.fetch_add(1, Ordering::Relaxed) >= INTERRUPT_LIMIT
                || started.elapsed() >= TIME_LIMIT
            {
                return Err(mlua::Error::runtime("widget execution limit exceeded"));
            }
            Ok(VmState::Continue)
        });

        let json = lua.create_table()?;
        json.set(
            "decode",
            lua.create_function(|lua, input: String| {
                let value: JsonValue =
                    serde_json::from_str(&input).map_err(mlua::Error::external)?;
                lua.to_value(&value)
            })?,
        )?;
        json.set(
            "encode",
            lua.create_function(|lua, value: Value| {
                let value: JsonValue = lua.from_value(value)?;
                serde_json::to_string(&value).map_err(mlua::Error::external)
            })?,
        )?;
        lua.globals().set("json", json)?;
        let settings = lua.to_value(&widget.settings)?;
        freeze_tables(&settings)?;
        lua.globals().set("settings", settings)?;

        let log_id = widget.id.clone();
        lua.globals().set(
            "log",
            lua.create_function(move |_, message: String| {
                tracing::info!(widget = %log_id, "{message}");
                Ok(())
            })?,
        )?;

        let allowed = widget.network_allow.clone();
        let http = lua.create_table()?;
        http.set(
            "get",
            lua.create_function(move |lua, (url, headers): (String, Option<Table>)| {
                let headers = parse_headers(headers)?;
                let response =
                    get_allowed(&client, &allowed, &url, headers).map_err(mlua::Error::external)?;
                lua.to_value(&response)
            })?,
        )?;
        lua.globals().set("http", http)?;
        lua.sandbox(true)?;

        let result: Value = lua
            .load(&widget.source)
            .set_name(&widget.id)
            .call(())
            .context("Lua widget failed")?;
        let content: StatsContent = lua
            .from_value(result)
            .context("Lua widget returned an invalid stats model")?;
        validate_content(&content)?;
        Ok(content)
    }
}

#[derive(Serialize)]
struct HttpResponse {
    status: u16,
    body: String,
}

fn parse_headers(headers: Option<Table>) -> mlua::Result<HeaderMap> {
    let mut result = HeaderMap::new();
    if let Some(headers) = headers {
        for pair in headers.pairs::<String, String>() {
            let (name, value) = pair?;
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(mlua::Error::external)?;
            if matches!(name.as_str(), "host" | "content-length" | "connection") {
                return Err(mlua::Error::runtime("restricted HTTP header"));
            }
            result.insert(
                name,
                HeaderValue::from_str(&value).map_err(mlua::Error::external)?,
            );
        }
    }
    Ok(result)
}

fn get_allowed(
    client: &Client,
    allowed: &[Url],
    initial: &str,
    mut headers: HeaderMap,
) -> Result<HttpResponse> {
    let mut url = validate_http_url(initial)?;
    for redirect_count in 0..=MAX_REDIRECTS {
        ensure_allowed(&url, allowed)?;
        let response = client.get(url.clone()).headers(headers.clone()).send()?;
        if is_redirect(response.status()) {
            if redirect_count == MAX_REDIRECTS {
                bail!("too many redirects");
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| anyhow!("redirect response did not include a location"))?
                .to_str()?;
            let next_url = url.join(location)?;
            if !same_origin(&url, &next_url) {
                strip_cross_origin_headers(&mut headers);
            }
            url = next_url;
            continue;
        }
        if let Some(size) = response.content_length()
            && size > MAX_BODY_BYTES as u64
        {
            bail!("HTTP response exceeded 1 MiB");
        }
        let status = response.status().as_u16();
        let mut bytes = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or_default()
                .min(MAX_BODY_BYTES as u64) as usize,
        );
        response
            .take((MAX_BODY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BODY_BYTES {
            bail!("HTTP response exceeded 1 MiB");
        }
        let body = String::from_utf8(bytes).context("HTTP response was not UTF-8")?;
        return Ok(HttpResponse { status, body });
    }
    unreachable!()
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

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn strip_cross_origin_headers(headers: &mut HeaderMap) {
    // A script can use any header name for a credential. Preserve only content
    // negotiation headers when redirecting to a different origin.
    let accept = headers.remove(ACCEPT);
    let accept_language = headers.remove(ACCEPT_LANGUAGE);
    headers.clear();
    if let Some(value) = accept {
        headers.insert(ACCEPT, value);
    }
    if let Some(value) = accept_language {
        headers.insert(ACCEPT_LANGUAGE, value);
    }
}

fn ensure_allowed(url: &Url, allowed: &[Url]) -> Result<()> {
    if !allowed.iter().any(|candidate| same_origin(candidate, url)) {
        bail!("outbound request origin is not allowed");
    }
    Ok(())
}

fn validate_content(content: &StatsContent) -> Result<()> {
    if content.title.trim().is_empty() {
        bail!("widget title cannot be empty");
    }
    if content.metrics.is_empty() || content.metrics.len() > 8 {
        bail!("widget must return between 1 and 8 metrics");
    }
    if let Some(href) = &content.href {
        validate_http_url(href).context("invalid widget link")?;
    }
    Ok(())
}

fn freeze_tables(value: &Value) -> mlua::Result<()> {
    if let Value::Table(table) = value {
        for pair in table.clone().pairs::<Value, Value>() {
            let (key, value) = pair?;
            freeze_tables(&key)?;
            freeze_tables(&value)?;
        }
        table.set_readonly(true);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_matching_includes_port() {
        let allowed = vec![Url::parse("https://example.com:8443").unwrap()];
        assert!(
            ensure_allowed(
                &Url::parse("https://example.com:8443/data").unwrap(),
                &allowed
            )
            .is_ok()
        );
        assert!(
            ensure_allowed(&Url::parse("https://example.com/data").unwrap(), &allowed).is_err()
        );
    }

    #[test]
    fn only_actionable_redirects_are_followed() {
        for status in [301, 302, 303, 307, 308] {
            assert!(is_redirect(StatusCode::from_u16(status).unwrap()));
        }
        assert!(!is_redirect(StatusCode::NOT_MODIFIED));
        assert!(!is_redirect(StatusCode::MULTIPLE_CHOICES));
    }

    #[test]
    fn cross_origin_redirects_drop_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
        headers.insert("cookie", HeaderValue::from_static("session=secret"));
        headers.insert("x-api-key", HeaderValue::from_static("secret"));
        headers.insert("x-auth-token", HeaderValue::from_static("secret"));
        headers.insert("accept", HeaderValue::from_static("application/json"));

        strip_cross_origin_headers(&mut headers);

        assert!(!headers.contains_key("authorization"));
        assert!(!headers.contains_key("cookie"));
        assert!(!headers.contains_key("x-api-key"));
        assert!(!headers.contains_key("x-auth-token"));
        assert_eq!(headers.get("accept").unwrap(), "application/json");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cpu_bound_widgets_do_not_block_the_async_runtime() {
        let widget = LuaWidget {
            id: "busy".into(),
            source: r#"
                local started = os.clock()
                while os.clock() - started < 0.05 do end
                return {
                    title = "Busy",
                    metrics = {{ label = "Count", value = 1 }},
                    fetched_at = "2026-01-01T00:00:00Z"
                }
            "#
            .into(),
            settings: std::collections::BTreeMap::new(),
            network_allow: vec![],
            columns: 4,
        };
        let runtime = LuaRuntime::new();
        let task = tokio::spawn(async move { runtime.execute(&widget).await });

        tokio::time::timeout(
            Duration::from_millis(20),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("CPU-bound Lua stalled the async runtime");
        let _ = task.await.unwrap();
    }

    #[tokio::test]
    async fn executes_a_sandboxed_stats_widget() {
        let widget = LuaWidget {
            id: "test".into(),
            source: r#"
                assert(io == nil)
                assert(package == nil)
                return {
                    title = "Test",
                    metrics = {{ label = "Count", value = settings.count }},
                    fetched_at = "2026-01-01T00:00:00Z"
                }
            "#
            .into(),
            settings: std::collections::BTreeMap::from([("count".into(), JsonValue::from(3))]),
            network_allow: vec![],
            columns: 4,
        };

        let content = LuaRuntime::new().execute(&widget).await.unwrap();
        assert_eq!(content.title, "Test");
        assert_eq!(content.metrics[0].value, JsonValue::from(3));
    }
}
