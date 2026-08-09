mod api;
mod config;
mod lua;

use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use axum::{
    Router,
    routing::{get, post},
};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

use config::LoadedConfig;
use lua::LuaRuntime;

pub struct AppState {
    config: LoadedConfig,
    lua: LuaRuntime,
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
    let listen = env::var("STELLE_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let address: SocketAddr = listen.parse().context("invalid STELLE_LISTEN address")?;

    let state = Arc::new(AppState {
        config: config::load(&config_path)?,
        lua: LuaRuntime::new(),
    });
    let index = static_dir.join("index.html");
    let static_files = ServeDir::new(&static_dir).not_found_service(ServeFile::new(index));
    let app = Router::new()
        .route("/api/dashboard", get(api::dashboard))
        .route("/api/widgets/{id}/refresh", post(api::refresh_widget))
        .route("/healthz", get(api::health))
        .fallback_service(static_files)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    tracing::info!(%address, config = %config_path.display(), "stelle is ready");
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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
