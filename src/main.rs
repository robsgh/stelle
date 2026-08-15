mod api;
mod config;
mod favicon;
mod lua;
mod traefik;

use std::{
    collections::HashMap,
    env,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Instant,
};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
};
use notify_debouncer_mini::notify::{
    Error as NotifyError, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    recommended_watcher,
};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

use config::{LinkWidget, LoadedConfig, TraefikWidget};
use lua::{LuaRuntime, StatsContent};

struct CachedWidget {
    widget: config::LuaWidget,
    content: StatsContent,
    refreshed_at: Instant,
}

struct CachedDiscovery {
    widget: TraefikWidget,
    links: Vec<LinkWidget>,
    refreshed_at: Instant,
}

type WidgetCache = Arc<RwLock<HashMap<String, CachedWidget>>>;
type DiscoveryCache = Arc<RwLock<HashMap<String, CachedDiscovery>>>;

const NO_CACHE: &str = "no-cache";
const NO_STORE: &str = "no-store";
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
const FAVICON_CACHE: &str = "public, max-age=21600, stale-if-error=86400";
const FAVICON_MISS_CACHE: &str = "public, max-age=900";

pub struct AppState {
    config: Arc<RwLock<LoadedConfig>>,
    lua: LuaRuntime,
    widget_cache: WidgetCache,
    discovery_cache: DiscoveryCache,
    favicons: Arc<favicon::Service>,
    widget_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "stelle=info,tower_http=info".into()),
        )
        .init();

    let config_path = PathBuf::from(
        env::var("STELLE_CONFIG").unwrap_or_else(|_| "/config/dashboard.yaml".into()),
    );
    let static_dir =
        PathBuf::from(env::var("STELLE_STATIC_DIR").unwrap_or_else(|_| "/app/public".into()));
    let port: u16 = env::var("STELLE_PORT")
        .unwrap_or_else(|_| "8080".into())
        .parse()
        .context("invalid STELLE_PORT")?;
    let address = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));

    let config = Arc::new(RwLock::new(config::load(&config_path)?));
    let widget_cache = Arc::new(RwLock::new(HashMap::new()));
    let discovery_cache = Arc::new(RwLock::new(HashMap::new()));
    let favicons = Arc::new(favicon::Service::new()?);
    let _config_watcher = watch_config(
        &config_path,
        Arc::clone(&config),
        Arc::clone(&widget_cache),
        Arc::clone(&discovery_cache),
        Arc::clone(&favicons),
    )?;
    let state = Arc::new(AppState {
        config,
        lua: LuaRuntime::new(),
        widget_cache,
        discovery_cache,
        favicons,
        widget_locks: Mutex::new(HashMap::new()),
    });
    let app = build_app(state, &static_dir);

    tracing::info!(%address, config = %config_path.display(), "stelle is ready");
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn build_app(state: Arc<AppState>, static_dir: &Path) -> Router {
    let index = static_dir.join("index.html");
    let static_files = ServeDir::new(static_dir).not_found_service(ServeFile::new(index));
    Router::new()
        .route("/api/dashboard", get(api::dashboard))
        .route("/api/favicon", get(api::favicon))
        .route("/api/widgets/{id}", get(api::widget))
        .route("/api/widgets/{id}/refresh", post(api::refresh_widget))
        .route("/healthz", get(api::health))
        .fallback_service(static_files)
        .layer(middleware::from_fn(cache_control))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn cache_control(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let policy = cache_policy(&path, response.status());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
    response
}

fn cache_policy(path: &str, status: StatusCode) -> &'static str {
    if path == "/api/favicon" && status.is_success() {
        FAVICON_CACHE
    } else if path == "/api/favicon" {
        FAVICON_MISS_CACHE
    } else if path.starts_with("/api/") || path == "/healthz" {
        NO_STORE
    } else if status.is_success() && path.starts_with("/_app/immutable/") {
        IMMUTABLE_CACHE
    } else {
        NO_CACHE
    }
}

fn watch_config(
    path: &Path,
    config: Arc<RwLock<LoadedConfig>>,
    widget_cache: WidgetCache,
    discovery_cache: DiscoveryCache,
    favicons: Arc<favicon::Service>,
) -> Result<RecommendedWatcher> {
    let config_path = path.to_owned();
    let mut watcher = recommended_watcher(
        move |event: std::result::Result<Event, NotifyError>| match event {
            Ok(event)
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) =>
            {
                match config::load(&config_path) {
                    Ok(updated) => {
                        *config.write().expect("configuration lock poisoned") = updated;
                        widget_cache
                            .write()
                            .expect("widget cache lock poisoned")
                            .clear();
                        discovery_cache
                            .write()
                            .expect("discovery cache lock poisoned")
                            .clear();
                        favicons.clear();
                        tracing::info!(config = %config_path.display(), "configuration reloaded");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "configuration reload failed; keeping previous configuration");
                    }
                }
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "configuration watch failed"),
        },
    )?;
    watcher.watch(
        path.parent().unwrap_or_else(|| Path::new(".")),
        RecursiveMode::Recursive,
    )?;
    Ok(watcher)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler")
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;
    use crate::{
        config::{Dashboard, LinkWidget, LoadedWidget, LuaWidget, Theme},
        lua::Metric,
    };

    fn test_state(widgets: Vec<LoadedWidget>) -> Arc<AppState> {
        let config = Dashboard {
            title: Some("Test".into()),
            subtitle: None,
            theme: Theme::System,
            accent: "#8b5cf6".into(),
            widgets,
        };
        Arc::new(AppState {
            config: Arc::new(RwLock::new(config)),
            lua: LuaRuntime::new(),
            widget_cache: Arc::new(RwLock::new(HashMap::new())),
            discovery_cache: Arc::new(RwLock::new(HashMap::new())),
            favicons: Arc::new(favicon::Service::new().unwrap()),
            widget_locks: Mutex::new(HashMap::new()),
        })
    }

    fn test_app(widgets: Vec<LoadedWidget>) -> Router {
        build_app(test_state(widgets), Path::new("frontend/build"))
    }

    async fn json_body(response: axum::response::Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn health_endpoint_reports_ok() {
        let response = test_app(vec![])
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await,
            serde_json::json!({ "status": "ok" })
        );
    }

    #[tokio::test]
    async fn dashboard_endpoint_omits_private_widget_data() {
        let widget = LoadedWidget::Lua(LuaWidget {
            id: "private".into(),
            cache_ttl: 300,
            source: "return {}".into(),
            settings: BTreeMap::from([("token".into(), Value::from("secret"))]),
            network_allow: vec![url::Url::parse("https://example.com").unwrap()],
        });
        let response = test_app(vec![widget])
            .oneshot(Request::get("/api/dashboard").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], NO_STORE);
        let body = json_body(response).await;
        assert_eq!(
            body["widgets"][0],
            serde_json::json!({ "type": "lua", "id": "private" })
        );
        assert!(!body.to_string().contains("secret"));
        assert!(!body.to_string().contains("example.com"));
    }

    #[test]
    fn cache_policy_revalidates_html_and_missing_assets() {
        assert_eq!(cache_policy("/", StatusCode::OK), NO_CACHE);
        assert_eq!(
            cache_policy("/_app/immutable/assets/old.css", StatusCode::NOT_FOUND),
            NO_CACHE
        );
    }

    #[test]
    fn cache_policy_keeps_successful_fingerprinted_assets_immutable() {
        assert_eq!(
            cache_policy("/_app/immutable/assets/app.abc123.css", StatusCode::OK),
            IMMUTABLE_CACHE
        );
    }

    #[test]
    fn cache_policy_caches_favicon_hits_and_misses() {
        assert_eq!(cache_policy("/api/favicon", StatusCode::OK), FAVICON_CACHE);
        assert_eq!(
            cache_policy("/api/favicon", StatusCode::NOT_FOUND),
            FAVICON_MISS_CACHE
        );
    }

    #[tokio::test]
    async fn favicon_endpoint_rejects_unconfigured_origins() {
        let response = test_app(vec![])
            .oneshot(
                Request::get("/api/favicon?url=https%3A%2F%2Fexample.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            FAVICON_MISS_CACHE
        );
    }

    #[tokio::test]
    async fn favicon_endpoint_sniffs_and_caches_configured_icons() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let hits = Arc::new(AtomicUsize::new(0));
        let upstream_hits = Arc::clone(&hits);
        let upstream = Router::new().route(
            "/favicon.ico",
            get(move || {
                let upstream_hits = Arc::clone(&upstream_hits);
                async move {
                    upstream_hits.fetch_add(1, Ordering::Relaxed);
                    (
                        [(header::CONTENT_TYPE, "text/html")],
                        b"\x89PNG\r\n\x1a\nmock".to_vec(),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let page_url = format!("http://{address}/app");
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("url", &page_url)
            .finish();
        let app = test_app(vec![LoadedWidget::Link(LinkWidget {
            label: "Upstream".into(),
            description: String::new(),
            url: page_url,
            favicon_url: None,
            accent: None,
        })]);

        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/api/favicon?{query}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
            assert_eq!(
                response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
            assert_eq!(response.headers()[header::CACHE_CONTROL], FAVICON_CACHE);
        }
        assert_eq!(hits.load(Ordering::Relaxed), 1);
        server.abort();
    }

    #[tokio::test]
    async fn widget_failures_return_the_public_api_error() {
        let widget = LoadedWidget::Lua(LuaWidget {
            id: "failing".into(),
            cache_ttl: 300,
            source: "error('private failure detail')".into(),
            settings: BTreeMap::new(),
            network_allow: vec![],
        });
        let response = test_app(vec![widget])
            .oneshot(
                Request::post("/api/widgets/failing/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            json_body(response).await,
            serde_json::json!({
                "error": {
                    "code": "widget_execution_failed",
                    "message": "The widget could not be refreshed"
                }
            })
        );
    }

    #[tokio::test]
    async fn widget_get_reuses_a_fresh_cached_result() {
        let widget = LoadedWidget::Lua(LuaWidget {
            id: "cached".into(),
            cache_ttl: 300,
            source: r#"
                return {
                    title = "Cached",
                    metrics = {{ label = "Count", value = 1 }},
                    fetched_at = "2026-01-01T00:00:00Z"
                }
            "#
            .into(),
            settings: BTreeMap::new(),
            network_allow: vec![],
        });
        let app = test_app(vec![widget]);

        let first = app
            .clone()
            .oneshot(
                Request::get("/api/widgets/cached")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let second = app
            .oneshot(
                Request::get("/api/widgets/cached")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(json_body(first).await["cache"]["cached"], false);
        assert_eq!(json_body(second).await["cache"]["cached"], true);
    }

    #[tokio::test]
    async fn expired_cache_falls_back_to_last_success_after_failure() {
        let widget = LuaWidget {
            id: "stale".into(),
            cache_ttl: 10,
            source: "error('temporary failure')".into(),
            settings: BTreeMap::new(),
            network_allow: vec![],
        };
        let state = test_state(vec![LoadedWidget::Lua(widget.clone())]);
        state.widget_cache.write().unwrap().insert(
            widget.id.clone(),
            CachedWidget {
                widget,
                content: StatsContent {
                    title: "Last success".into(),
                    subtitle: String::new(),
                    href: None,
                    metrics: vec![Metric {
                        label: "Count".into(),
                        value: Value::from(1),
                    }],
                    fetched_at: "2026-01-01T00:00:00Z".into(),
                },
                refreshed_at: Instant::now() - Duration::from_secs(11),
            },
        );
        let response = build_app(state, Path::new("frontend/build"))
            .oneshot(
                Request::get("/api/widgets/stale")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["content"]["title"], "Last success");
        assert_eq!(body["cache"]["stale"], true);
    }
}
