use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{AppState, config::LoadedWidget, lua::TIME_LIMIT};

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

pub async fn refresh_widget(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let widget = state
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
        ))?;
    let content = tokio::time::timeout(TIME_LIMIT, state.lua.execute(&widget))
        .await
        .map_err(|_| {
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
        })?;
    Ok(Json(
        json!({ "widget_id": id, "kind": "stats", "content": content }),
    ))
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
