use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    AppState, CachedWidget,
    config::{LoadedWidget, LuaWidget},
    lua::{StatsContent, TIME_LIMIT},
};

pub async fn dashboard(State(state): State<Arc<AppState>>) -> Response {
    Json(
        state
            .config
            .read()
            .expect("configuration lock poisoned")
            .clone(),
    )
    .into_response()
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
