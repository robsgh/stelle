mod api;
mod config;
mod lua;

use std::{
    env,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Router,
    routing::{get, post},
};
use notify_debouncer_mini::{
    DebounceEventResult, Debouncer, new_debouncer,
    notify::{RecommendedWatcher, RecursiveMode},
};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

use config::LoadedConfig;
use lua::LuaRuntime;

pub struct AppState {
    config: Arc<RwLock<LoadedConfig>>,
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
    let port: u16 = env::var("STELLE_PORT")
        .unwrap_or_else(|_| "8080".into())
        .parse()
        .context("invalid STELLE_PORT")?;
    let address = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));

    let config = Arc::new(RwLock::new(config::load(&config_path)?));
    let _config_watcher = watch_config(&config_path, Arc::clone(&config))?;
    let state = Arc::new(AppState {
        config,
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

fn watch_config(
    path: &Path,
    config: Arc<RwLock<LoadedConfig>>,
) -> Result<Debouncer<RecommendedWatcher>> {
    let config_path = path.to_owned();
    let mut watcher = new_debouncer(
        Duration::from_millis(500),
        move |event: DebounceEventResult| match event {
            Ok(_) => match config::load(&config_path) {
                Ok(updated) => {
                    *config.write().expect("configuration lock poisoned") = updated;
                    tracing::info!(config = %config_path.display(), "configuration reloaded");
                }
                Err(error) => {
                    tracing::warn!(%error, "configuration reload failed; keeping previous configuration");
                }
            },
            Err(error) => tracing::warn!(%error, "configuration watch failed"),
        },
    )?;
    watcher.watcher().watch(
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
