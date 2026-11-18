use std::path::PathBuf;

use set_server::{db, router, setup};

#[tokio::main]
async fn main() -> Result<(), db::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let db_path = std::env::var("SET_DB").unwrap_or_else(|_| "set.db".into());
    let static_dir = PathBuf::from(std::env::var("SET_STATIC").unwrap_or_else(|_| "web/dist".into()));
    let addr = std::env::var("SET_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let state = setup(&db_path).await?;
    if let Some(code) = db::bootstrap_code(&state.db).await? {
        tracing::info!("No players yet. Register the first one at /register?code={code}");
    }
    if !static_dir.join("index.html").exists() {
        tracing::warn!("{} has no index.html; only the API is served", static_dir.display());
    }

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, router(state, Some(&static_dir))).await?;
    Ok(())
}
