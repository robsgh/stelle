use std::{collections::HashSet, sync::Arc, time::Duration};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    AppState,
    config::{LinkWidget, LoadedWidget, LuaWidget, TraefikWidget, validate_http_url},
    lua::{StatsContent, TIME_LIMIT},
    network, traefik,
};

#[derive(Deserialize)]
pub struct FaviconQuery {
    url: String,
}

struct FaviconSource {
    override_url: Option<url::Url>,
}

pub async fn favicon(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FaviconQuery>,
) -> Response {
    let Ok(page_url) = validate_http_url(&query.url) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !page_url.username().is_empty() || page_url.password().is_some() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(source) = favicon_source_for_link(&state, &page_url) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(icon) = state
        .favicons
        .get(&page_url, source.override_url.as_ref())
        .await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut response = Response::new(Body::from(icon.bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(icon.content_type),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn favicon_source_for_link(state: &AppState, page_url: &url::Url) -> Option<FaviconSource> {
    let configured = {
        let config = state.config.read().expect("configuration lock poisoned");
        favicon_source(
            config.widgets.iter().filter_map(|widget| match widget {
                LoadedWidget::Link(link) => Some(link),
                _ => None,
            }),
            page_url,
        )
    };
    if configured.is_some() {
        return configured;
    }

    state
        .discovery_cache
        .find_map(|links| favicon_source(links, page_url))
}

fn favicon_source<'a>(
    links: impl IntoIterator<Item = &'a LinkWidget>,
    page_url: &url::Url,
) -> Option<FaviconSource> {
    let mut origin_match = None;
    for link in links {
        let source = || FaviconSource {
            override_url: link.favicon_url.clone(),
        };
        if link.url == *page_url {
            return Some(source());
        }
        if origin_match.is_none() && network::same_origin(&link.url, page_url) {
            origin_match = Some(source());
        }
    }
    origin_match
}

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
            LoadedWidget::Link(link) => link_host(link),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut widgets = Vec::new();

    for widget in dashboard.widgets {
        match widget {
            LoadedWidget::Traefik(discovery) => {
                for link in discovered_links(&state, &discovery).await {
                    push_unique_link(&mut hosts, &mut widgets, link);
                }
            }
            widget => widgets.push(widget),
        }
    }
    dashboard.widgets = widgets;
    Json(dashboard).into_response()
}

fn link_host(link: &LinkWidget) -> Option<String> {
    link.url.host_str().map(str::to_ascii_lowercase)
}

fn push_unique_link(
    hosts: &mut HashSet<String>,
    widgets: &mut Vec<LoadedWidget>,
    link: LinkWidget,
) {
    if link_host(&link).is_some_and(|host| hosts.insert(host)) {
        widgets.push(LoadedWidget::Link(link));
    }
}

async fn discovered_links(state: &AppState, widget: &TraefikWidget) -> Vec<LinkWidget> {
    if let Some(cached) = cached_discovery(state, widget)
        && cached.fresh
    {
        return cached.value;
    }

    let lock = state.refresh_locks.get(&widget.id);
    let _guard = lock.lock().await;
    if let Some(cached) = cached_discovery(state, widget)
        && cached.fresh
    {
        return cached.value;
    }

    let stale = cached_discovery(state, widget);
    match traefik::discover(widget).await {
        Ok(links) => {
            store_discovery(state, widget, links.clone());
            links
        }
        Err(error) => {
            if let Some(stale) = stale {
                tracing::warn!(widget = %widget.id, %error, "Traefik discovery failed; using stale links");
                stale.value
            } else {
                tracing::warn!(widget = %widget.id, %error, "Traefik discovery failed");
                Vec::new()
            }
        }
    }
}

fn cached_discovery(
    state: &AppState,
    widget: &TraefikWidget,
) -> Option<crate::cache::Hit<Vec<LinkWidget>>> {
    state
        .discovery_cache
        .get(&widget.id, widget, Duration::from_secs(widget.cache_ttl))
}

fn store_discovery(state: &AppState, widget: &TraefikWidget, links: Vec<LinkWidget>) {
    let config = state.config.read().expect("configuration lock poisoned");
    let still_current = config
        .widgets
        .iter()
        .any(|candidate| matches!(candidate, LoadedWidget::Traefik(current) if current == widget));
    if still_current {
        state
            .discovery_cache
            .insert(widget.id.clone(), widget.clone(), links);
    }
}

pub async fn widget(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let widget = find_widget(&state, &id)?;
    if let Some(cached) = cached_widget(&state, &widget)
        && cached.fresh
    {
        return Ok(widget_response(&id, cached.value, true, false));
    }

    let lock = state.refresh_locks.get(&id);
    let _guard = lock.lock().await;
    if let Some(cached) = cached_widget(&state, &widget)
        && cached.fresh
    {
        return Ok(widget_response(&id, cached.value, true, false));
    }

    let stale = cached_widget(&state, &widget);
    match execute_widget(&state, &id, &widget).await {
        Ok(content) => {
            store_widget(&state, &widget, content.clone());
            Ok(widget_response(&id, content, false, false))
        }
        Err(error) => match stale {
            Some(stale) => Ok(widget_response(&id, stale.value, true, true)),
            None => Err(error),
        },
    }
}

pub async fn refresh_widget(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let widget = find_widget(&state, &id)?;
    let lock = state.refresh_locks.get(&id);
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

fn cached_widget(state: &AppState, widget: &LuaWidget) -> Option<crate::cache::Hit<StatsContent>> {
    state
        .widget_cache
        .get(&widget.id, widget, Duration::from_secs(widget.cache_ttl))
}

fn store_widget(state: &AppState, widget: &LuaWidget, content: StatsContent) {
    let config = state.config.read().expect("configuration lock poisoned");
    let still_current = config
        .widgets
        .iter()
        .any(|candidate| matches!(candidate, LoadedWidget::Lua(current) if current == widget));
    if still_current {
        state
            .widget_cache
            .insert(widget.id.clone(), widget.clone(), content);
    }
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

pub async fn not_found() -> ApiError {
    ApiError(
        StatusCode::NOT_FOUND,
        "endpoint_not_found",
        "API endpoint was not found",
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn link(label: &str, url: &str) -> LinkWidget {
        LinkWidget {
            label: label.into(),
            description: String::new(),
            url: url::Url::parse(url).unwrap(),
            favicon_url: None,
            accent: None,
        }
    }

    #[test]
    fn discovered_links_do_not_duplicate_hardcoded_hosts() {
        let mut hosts = HashSet::from(["registry.example.com".into()]);
        let mut widgets = Vec::new();

        push_unique_link(
            &mut hosts,
            &mut widgets,
            link("Discovered Registry", "https://registry.example.com/other"),
        );
        push_unique_link(
            &mut hosts,
            &mut widgets,
            link("New Service", "https://new.example.com/"),
        );

        assert_eq!(widgets.len(), 1);
        assert!(matches!(
            &widgets[0],
            LoadedWidget::Link(link) if link.label == "New Service"
        ));
    }

    #[test]
    fn exact_link_selects_its_own_favicon_override() {
        let links = [
            LinkWidget {
                favicon_url: Some(url::Url::parse("https://cdn.example.com/one.png").unwrap()),
                ..link("One", "https://example.com/one")
            },
            LinkWidget {
                favicon_url: Some(url::Url::parse("https://cdn.example.com/two.png").unwrap()),
                ..link("Two", "https://example.com/two")
            },
        ];

        let source = favicon_source(&links, &url::Url::parse("https://example.com/two").unwrap())
            .expect("configured favicon source");
        assert_eq!(
            source.override_url.as_ref().map(url::Url::as_str),
            Some("https://cdn.example.com/two.png")
        );
    }
}
