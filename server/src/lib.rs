pub mod api;
pub mod auth;
pub mod db;
pub mod hub;
pub mod mail;
pub mod rules;
pub mod ws;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use sqlx::SqlitePool;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub hub: Arc<Mutex<hub::Hub>>,
    pub mailer: mail::Mailer,
}

/// Opens the database and starts the background sweeper for idle games.
pub async fn setup(db_path: &str, mailer: mail::Mailer) -> Result<AppState, db::Error> {
    let db = db::open(db_path).await?;
    let hub = Arc::new(Mutex::new(hub::Hub::new(db.clone())));
    let sweeper = Arc::downgrade(&hub);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            let Some(hub) = sweeper.upgrade() else { break };
            hub.lock().unwrap().sweep();
        }
    });
    Ok(AppState { db, hub, mailer })
}

/// The API and WebSocket routes, plus the built frontend in `static_dir` when given.
/// Unknown paths serve `index.html` so the client-side router handles them.
pub fn router(state: AppState, static_dir: Option<&Path>) -> Router {
    let mut app = api::routes();
    if let Some(dir) = static_dir {
        app = app.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html"))));
    }
    app.with_state(state)
}
