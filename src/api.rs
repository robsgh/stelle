use std::{collections::HashSet, sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    AppState, CachedDiscovery, CachedWidget,
    config::{LinkWidget, LoadedWidget, LuaWidget, TraefikWidget},
    lua::{StatsContent, TIME_LIMIT},
    traefik,
};

pub async fn dashboard(State(state): State<Arc<AppState>>) -> Response {
    let mut dashboard = state
        .config
        .read()
        .expect("configuration lock poisoned")
        .clone();
    let mut hosts = dashboard
        .widgets
        .iter()
        .filter_map(|widget| match widget {
            LoadedWidget::Link(link) => url::Url::parse(&link.url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_ascii_lowercase)),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut widgets = Vec::new();

    for widget in dashboard.widgets {
        match widget {
            LoadedWidget::Traefik(discovery) => {
                for link in discovered_links(&state, &discovery).await {
                    let Some(host) = url::Url::parse(&link.url)
                        .ok()
                        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
                    else {
                        continue;
                    };
                    if hosts.insert(host) {
                        widgets.push(LoadedWidget::Link(link));
                    }
                }
            }
            widget => widgets.push(widget),
        }
    }
    dashboard.widgets = widgets;
    Json(dashboard).into_response()
}

struct DiscoveryHit {
    links: Vec<LinkWidget>,
    fresh: bool,
}

async fn discovered_links(state: &AppState, widget: &TraefikWidget) -> Vec<LinkWidget> {
    if let Some(cached) = cached_discovery(state, widget)
        && cached.fresh
    {
        return cached.links;
    }

    let lock = widget_lock(state, &widget.id);
    let _guard = lock.lock().await;
    if let Some(cached) = cached_discovery(state, widget)
        && cached.fresh
    {
        return cached.links;
    }

    let stale = cached_discovery(state, widget);
    match traefik::discover(widget).await {
        Ok(links) => {
            store_discovery(state, widget, links.clone());
            links
        }
        Err(error) if stale.is_some() => {
            tracing::warn!(widget = %widget.id, %error, "Traefik discovery failed; using stale links");
            stale.expect("stale discovery disappeared").links
        }
        Err(error) => {
            tracing::warn!(widget = %widget.id, %error, "Traefik discovery failed");
            Vec::new()
        }
    }
}

fn cached_discovery(state: &AppState, widget: &TraefikWidget) -> Option<DiscoveryHit> {
    let cache = state
        .discovery_cache
        .read()
        .expect("discovery cache lock poisoned");
    let cached = cache.get(&widget.id)?;
    if cached.widget != *widget {
        return None;
    }
    Some(DiscoveryHit {
        links: cached.links.clone(),
        fresh: cached.refreshed_at.elapsed() < Duration::from_secs(widget.cache_ttl),
    })
}

fn store_discovery(state: &AppState, widget: &TraefikWidget, links: Vec<LinkWidget>) {
    let still_current = state
        .config
        .read()
        .expect("configuration lock poisoned")
        .widgets
        .iter()
        .any(|candidate| matches!(candidate, LoadedWidget::Traefik(current) if current == widget));
    if !still_current {
        return;
    }
    state
        .discovery_cache
        .write()
        .expect("discovery cache lock poisoned")
        .insert(
            widget.id.clone(),
            CachedDiscovery {
                widget: widget.clone(),
                links,
                refreshed_at: std::time::Instant::now(),
            },
        );
}

pub async fn widget(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let widget = find_widget(&state, &id)?;
    if let Some(cached) = cached_widget(&state, &widget)
        && cached.fresh
    {
        return Ok(widget_response(&id, cached.content, true, false));
    }

    let lock = widget_lock(&state, &id);
    let _guard = lock.lock().await;
    if let Some(cached) = cached_widget(&state, &widget)
        && cached.fresh
    {
        return Ok(widget_response(&id, cached.content, true, false));
    }

    let stale = cached_widget(&state, &widget);
    match execute_widget(&state, &id, &widget).await {
        Ok(content) => {
            store_widget(&state, &widget, content.clone());
            Ok(widget_response(&id, content, false, false))
        }
        Err(_) if stale.is_some() => Ok(widget_response(
            &id,
            stale.expect("stale cache entry disappeared").content,
            true,
            true,
        )),
        Err(error) => Err(error),
    }
}

pub async fn refresh_widget(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let widget = find_widget(&state, &id)?;
    let lock = widget_lock(&state, &id);
    let _guard = lock.lock().await;
    let content = execute_widget(&state, &id, &widget).await?;
    store_widget(&state, &widget, content.clone());
    Ok(widget_response(&id, content, false, false))
}

fn find_widget(state: &AppState, id: &str) -> Result<LuaWidget, ApiError> {
    state
        .config
        .read()
        .expect("configuration lock poisoned")
        .widgets
        .iter()
        .find_map(|widget| match widget {
            LoadedWidget::Lua(widget) if widget.id == id => Some(widget.clone()),
            _ => None,
        })
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "widget_not_found",
            "Widget was not found",
        ))
}

async fn execute_widget(
    state: &AppState,
    id: &str,
    widget: &LuaWidget,
) -> Result<StatsContent, ApiError> {
    tokio::time::timeout(TIME_LIMIT, state.lua.execute(widget))
        .await
        .map_err(|_| {
            tracing::warn!(widget = %id, "widget refresh timed out");
            ApiError(
                StatusCode::BAD_GATEWAY,
                "widget_timeout",
                "The widget timed out",
            )
        })?
        .map_err(|error| {
            tracing::warn!(widget = %id, error = %error, "widget refresh failed");
            ApiError(
                StatusCode::BAD_GATEWAY,
                "widget_execution_failed",
                "The widget could not be refreshed",
            )
        })
}

struct CacheHit {
    content: StatsContent,
    fresh: bool,
}

fn cached_widget(state: &AppState, widget: &LuaWidget) -> Option<CacheHit> {
    let cache = state
        .widget_cache
        .read()
        .expect("widget cache lock poisoned");
    let cached = cache.get(&widget.id)?;
    if cached.widget != *widget {
        return None;
    }
    Some(CacheHit {
        content: cached.content.clone(),
        fresh: cached.refreshed_at.elapsed() < Duration::from_secs(widget.cache_ttl),
    })
}

fn store_widget(state: &AppState, widget: &LuaWidget, content: StatsContent) {
    let still_current = state
        .config
        .read()
        .expect("configuration lock poisoned")
        .widgets
        .iter()
        .any(|candidate| matches!(candidate, LoadedWidget::Lua(current) if current == widget));
    if !still_current {
        return;
    }
    state
        .widget_cache
        .write()
        .expect("widget cache lock poisoned")
        .insert(
            widget.id.clone(),
            CachedWidget {
                widget: widget.clone(),
                content,
                refreshed_at: std::time::Instant::now(),
            },
        );
}

fn widget_lock(state: &AppState, id: &str) -> Arc<tokio::sync::Mutex<()>> {
    state
        .widget_locks
        .lock()
        .expect("widget lock map poisoned")
        .entry(id.to_owned())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn widget_response(
    id: &str,
    content: StatsContent,
    cached: bool,
    stale: bool,
) -> Json<serde_json::Value> {
    Json(json!({
        "widget_id": id,
        "kind": "stats",
        "content": content,
        "cache": { "cached": cached, "stale": stale }
    }))
}

pub async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

pub struct ApiError(StatusCode, &'static str, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(json!({ "error": { "code": self.1, "message": self.2 } })),
        )
            .into_response()
    }
}
