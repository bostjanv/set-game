use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{AppendHeaders, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{self, CurrentUser};
use crate::db::{self, now};
use crate::{ws, AppState};

pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("database error: {e}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Internal error".into())
    }
}

fn bad_request(msg: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

type ApiResult<T> = Result<T, ApiError>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/register", post(register))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/invites", get(list_invites).post(create_invite))
        .route("/api/players", get(players))
        .route("/api/leaderboard", get(leaderboard))
        .route("/ws", get(ws::handler))
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
    #[serde(default)]
    code: String,
}

async fn start_session(state: &AppState, user_id: i64, username: String) -> ApiResult<impl IntoResponse> {
    let token = db::random_token();
    sqlx::query("INSERT INTO sessions (token, user_id, created_at) VALUES (?, ?, ?)")
        .bind(&token)
        .bind(user_id)
        .bind(now())
        .execute(&state.db)
        .await?;
    Ok((
        AppendHeaders([(SET_COOKIE, auth::session_cookie(&token))]),
        Json(json!({ "id": user_id, "username": username })),
    ))
}

async fn register(State(state): State<AppState>, Json(body): Json<Credentials>) -> ApiResult<impl IntoResponse> {
    let username = body.username.trim().to_owned();
    let valid_name = (3..=20).contains(&username.chars().count())
        && username.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid_name {
        return Err(bad_request("Username must be 3 to 20 letters, digits, '_' or '-'"));
    }
    if body.password.chars().count() < 6 {
        return Err(bad_request("Password must have at least 6 characters"));
    }
    let password = body.password.clone();
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password)).await.unwrap();

    let mut tx = state.db.begin().await?;
    let code_ok: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM invite_codes WHERE code = ? AND used_by IS NULL")
            .bind(body.code.trim().to_uppercase())
            .fetch_optional(&mut *tx)
            .await?;
    if code_ok.is_none() {
        return Err(bad_request("This invite code is invalid or already used"));
    }
    let taken: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE username = ?")
        .bind(&username)
        .fetch_optional(&mut *tx)
        .await?;
    if taken.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "That username is taken".into()));
    }
    let user_id = sqlx::query("INSERT INTO users (username, password_hash, created_at) VALUES (?, ?, ?)")
        .bind(&username)
        .bind(hash)
        .bind(now())
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
    sqlx::query("UPDATE invite_codes SET used_by = ?, used_at = ? WHERE code = ?")
        .bind(user_id)
        .bind(now())
        .bind(body.code.trim().to_uppercase())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    start_session(&state, user_id, username).await
}

async fn login(State(state): State<AppState>, Json(body): Json<Credentials>) -> ApiResult<impl IntoResponse> {
    let row: Option<(i64, String, String)> =
        sqlx::query_as("SELECT id, username, password_hash FROM users WHERE username = ?")
            .bind(body.username.trim())
            .fetch_optional(&state.db)
            .await?;
    let invalid = || ApiError(StatusCode::UNAUTHORIZED, "Invalid username or password".into());
    let (id, username, hash) = row.ok_or_else(invalid)?;
    let password = body.password;
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash)).await.unwrap();
    if !ok {
        return Err(invalid());
    }
    start_session(&state, id, username).await
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    if let Some(token) = auth::session_token(&headers) {
        sqlx::query("DELETE FROM sessions WHERE token = ?").bind(token).execute(&state.db).await?;
    }
    Ok((AppendHeaders([(SET_COOKIE, auth::clear_cookie())]), StatusCode::NO_CONTENT))
}

async fn me(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Json<Value>> {
    let (played, wins): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(won), 0) FROM game_players WHERE user_id = ?")
            .bind(user.id)
            .fetch_one(&state.db)
            .await?;
    Ok(Json(json!({ "id": user.id, "username": user.username, "played": played, "wins": wins })))
}

async fn create_invite(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Json<Value>> {
    let code = db::random_code();
    sqlx::query("INSERT INTO invite_codes (code, created_by, created_at) VALUES (?, ?, ?)")
        .bind(&code)
        .bind(user.id)
        .bind(now())
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "code": code })))
}

async fn list_invites(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT c.code, c.created_at, u.username FROM invite_codes c
         LEFT JOIN users u ON u.id = c.used_by
         WHERE c.created_by = ? ORDER BY c.created_at DESC, c.rowid DESC",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let codes: Vec<Value> = rows
        .into_iter()
        .map(|(code, created_at, used_by)| json!({ "code": code, "created_at": created_at, "used_by": used_by }))
        .collect();
    Ok(Json(json!(codes)))
}

async fn players(State(state): State<AppState>, _user: CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, username FROM users ORDER BY username")
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!(rows.into_iter().map(|(id, username)| json!({ "id": id, "username": username })).collect::<Vec<_>>())))
}

async fn leaderboard(State(state): State<AppState>, _user: CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT u.username, COUNT(*) AS played, SUM(gp.won) AS wins
         FROM game_players gp JOIN users u ON u.id = gp.user_id
         GROUP BY u.id
         ORDER BY wins DESC, CAST(wins AS REAL) / played DESC, played DESC, u.username",
    )
    .fetch_all(&state.db)
    .await?;
    let board: Vec<Value> = rows
        .into_iter()
        .map(|(username, played, wins)| json!({ "username": username, "played": played, "wins": wins }))
        .collect();
    Ok(Json(json!(board)))
}
