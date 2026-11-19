use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{AppendHeaders, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;

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
        .route("/api/verify", post(verify))
        .route("/api/verify/resend", post(resend_verify))
        .route("/api/login", post(login))
        .route("/api/forgot", post(forgot))
        .route("/api/reset", post(reset))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/players", get(players))
        .route("/api/leaderboard", get(leaderboard))
        .route("/ws", get(ws::handler))
}

/// How long an emailed code works. Unconfirmed accounts without a working code are deleted.
const CODE_TTL: i64 = 30 * 60;
/// Wrong guesses allowed per emailed code, so it cannot be brute-forced.
const CODE_ATTEMPTS: i64 = 5;

#[derive(Deserialize)]
struct Registration {
    email: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct EmailOnly {
    email: String,
}

#[derive(Deserialize)]
struct CodeBody {
    code: String,
    /// The new password, for `reset`.
    #[serde(default)]
    password: String,
}

fn check_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < 6 {
        return Err(bad_request("Password must have at least 6 characters"));
    }
    Ok(())
}

async fn hash(password: String) -> String {
    tokio::task::spawn_blocking(move || auth::hash_password(&password)).await.unwrap()
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

fn new_code() -> String {
    format!("{:06}", rand::Rng::gen_range(&mut rand::thread_rng(), 0..1_000_000))
}

/// Stores a new code for the user's `purpose`, replacing an earlier one, and returns the secret
/// for the browser's cookie and the code to email.
async fn create_code(conn: &mut sqlx::SqliteConnection, user_id: i64, purpose: &str) -> ApiResult<(String, String)> {
    sqlx::query("DELETE FROM email_codes WHERE expires_at <= ? OR (user_id = ? AND purpose = ?)")
        .bind(now())
        .bind(user_id)
        .bind(purpose)
        .execute(&mut *conn)
        .await?;
    let (token, code) = (db::random_token(), new_code());
    sqlx::query("INSERT INTO email_codes (token, user_id, purpose, code, expires_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&token)
        .bind(user_id)
        .bind(purpose)
        .bind(&code)
        .bind(now() + CODE_TTL)
        .execute(conn)
        .await?;
    Ok((token, code))
}

/// Checks `code` against the one emailed to the browser whose `purpose` cookie the request
/// carries, and deletes it and returns its user when it matches. Without the cookie the code is
/// useless, so intercepting the email is not enough. Wrong reset codes all get the same answer,
/// so it does not reveal whether `/api/forgot` found an account.
async fn take_code(db: &SqlitePool, headers: &HeaderMap, purpose: &str, code: &str) -> ApiResult<i64> {
    let verify = purpose == "verify";
    let token = auth::cookie(headers, purpose).ok_or_else(|| {
        bad_request(if verify {
            "Enter the code in the browser where you registered, or register again here."
        } else {
            "Enter the code in the browser where you asked for it, or ask for a new one here."
        })
    })?;
    let invalid = || bad_request(if verify { "This code has expired. Send a new one." } else { "This code is wrong or has expired." });
    // Counts the guess before comparing, so concurrent guesses cannot exceed the limit.
    let (expected, attempts): (String, i64) = sqlx::query_as(
        "UPDATE email_codes SET attempts = attempts + 1
         WHERE token = ? AND purpose = ? AND expires_at > ? AND attempts < ? RETURNING code, attempts",
    )
    .bind(&token)
    .bind(purpose)
    .bind(now())
    .bind(CODE_ATTEMPTS)
    .fetch_optional(db)
    .await?
    .ok_or_else(invalid)?;
    if code.trim() != expected {
        let left = CODE_ATTEMPTS - attempts;
        return Err(if verify && left > 0 { bad_request(&format!("Wrong code. {left} attempts left.")) } else { invalid() });
    }
    // Deleting claims the code, so it works only once.
    sqlx::query_scalar("DELETE FROM email_codes WHERE token = ? RETURNING user_id")
        .bind(&token)
        .fetch_optional(db)
        .await?
        .ok_or_else(invalid)
}

/// Records a request to email `email`, or fails with 429 when the address already had one in
/// the last minute or five in the last hour. Callers record requests for unknown addresses
/// too, so the limit does not reveal which emails are registered.
async fn rate_limit_email(conn: &mut sqlx::SqliteConnection, email: &str) -> ApiResult<()> {
    let now = now();
    sqlx::query("DELETE FROM email_sends WHERE sent_at <= ?").bind(now - 3600).execute(&mut *conn).await?;
    // One statement, so concurrent requests cannot both pass the check.
    let recorded = sqlx::query(
        "INSERT INTO email_sends (email, sent_at) SELECT ?1, ?2
         WHERE NOT EXISTS (SELECT 1 FROM email_sends WHERE email = ?1 AND sent_at > ?2 - 60)
           AND (SELECT COUNT(*) FROM email_sends WHERE email = ?1) < 5",
    )
    .bind(email)
    .bind(now)
    .execute(conn)
    .await?
    .rows_affected();
    if recorded == 0 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "We sent an email to this address recently. Wait a few minutes and try again.".into(),
        ));
    }
    Ok(())
}

async fn send_mail(state: &AppState, to: &str, subject: &str, body: String) -> ApiResult<()> {
    state.mailer.send(to, subject, body).await.map_err(|e| {
        tracing::error!("sending email to {to}: {e}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Could not send the email. Try again later.".into())
    })
}

/// Creates an unverified account and emails its confirmation code; the account can log in
/// once the code is entered in this browser, which gets the `verify` cookie.
/// Registering an email that is not confirmed yet replaces that account.
async fn register(State(state): State<AppState>, Json(body): Json<Registration>) -> ApiResult<impl IntoResponse> {
    let email = body.email.trim().to_owned();
    if email.parse::<lettre::Address>().is_err() {
        return Err(bad_request("Enter a valid email address"));
    }
    let username = body.username.trim().to_owned();
    let valid_name = (3..=20).contains(&username.chars().count())
        && username.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid_name {
        return Err(bad_request("Username must be 3 to 20 letters, digits, '_' or '-'"));
    }
    check_password(&body.password)?;
    let hash = hash(body.password).await;

    let mut tx = state.db.begin().await?;
    // Unconfirmed accounts for this email, and unconfirmed ones whose code has expired, free their
    // email and username.
    sqlx::query(
        "DELETE FROM users WHERE email_verified_at IS NULL AND (email = ? OR NOT EXISTS
           (SELECT 1 FROM email_codes c WHERE c.user_id = users.id AND c.purpose = 'verify' AND c.expires_at > ?))",
    )
    .bind(&email)
    .bind(now())
    .execute(&mut *tx)
    .await?;
    let taken: Option<(String, String)> = sqlx::query_as("SELECT email, username FROM users WHERE email = ? OR username = ?")
        .bind(&email)
        .bind(&username)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some((taken_email, _)) = taken {
        let msg = if taken_email.eq_ignore_ascii_case(&email) {
            "That email is already registered. Log in or reset your password."
        } else {
            "That username is taken"
        };
        return Err(ApiError(StatusCode::CONFLICT, msg.into()));
    }
    // Inside the transaction, so a refused request leaves an earlier unconfirmed account in place.
    rate_limit_email(&mut tx, &email).await?;
    let user_id = sqlx::query("INSERT INTO users (email, username, password_hash, created_at) VALUES (?, ?, ?, ?)")
        .bind(&email)
        .bind(&username)
        .bind(hash)
        .bind(now())
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
    let (token, code) = create_code(&mut tx, user_id, "verify").await?;
    tx.commit().await?;
    send_mail(&state, &email, "Your SET confirmation code", verify_mail(&username, &code)).await?;
    Ok((AppendHeaders([(SET_COOKIE, auth::code_cookie("verify", &token, CODE_TTL))]), StatusCode::NO_CONTENT))
}

fn verify_mail(username: &str, code: &str) -> String {
    format!(
        "Hi {username},\n\nYour SET confirmation code is:\n\n{code}\n\n\
         Enter it on the page where you registered. It expires in 30 minutes.\n\
         If you did not register, ignore this email.\n"
    )
}

/// Confirms the email of the account this browser registered when `code` is the emailed one,
/// and logs it in.
async fn verify(State(state): State<AppState>, headers: HeaderMap, Json(body): Json<CodeBody>) -> ApiResult<impl IntoResponse> {
    let user_id = take_code(&state.db, &headers, "verify", &body.code).await?;
    let username: String = sqlx::query_scalar("UPDATE users SET email_verified_at = ? WHERE id = ? RETURNING username")
        .bind(now())
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    Ok((AppendHeaders([(SET_COOKIE, auth::code_cookie("verify", "", 0))]), start_session(&state, user_id, username).await?))
}

/// Emails a new confirmation code to the account this browser registered, with fresh attempts.
/// The browser keeps its cookie.
async fn resend_verify(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    let gone = || bad_request("This registration has expired. Register again.");
    let token = auth::cookie(&headers, "verify").ok_or_else(gone)?;
    let mut tx = state.db.begin().await?;
    let (email, username): (String, String) = sqlx::query_as(
        "SELECT u.email, u.username FROM email_codes c JOIN users u ON u.id = c.user_id WHERE c.token = ? AND c.purpose = 'verify'",
    )
    .bind(&token)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(gone)?;
    rate_limit_email(&mut tx, &email).await?;
    let code = new_code();
    sqlx::query("UPDATE email_codes SET code = ?, attempts = 0, expires_at = ? WHERE token = ?")
        .bind(&code)
        .bind(now() + CODE_TTL)
        .bind(&token)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    send_mail(&state, &email, "Your SET confirmation code", verify_mail(&username, &code)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn login(State(state): State<AppState>, Json(body): Json<Login>) -> ApiResult<impl IntoResponse> {
    let row: Option<(i64, String, String, Option<i64>)> =
        sqlx::query_as("SELECT id, username, password_hash, email_verified_at FROM users WHERE email = ?")
            .bind(body.email.trim())
            .fetch_optional(&state.db)
            .await?;
    let invalid = || ApiError(StatusCode::UNAUTHORIZED, "Invalid email or password".into());
    let (id, username, hash, verified_at) = row.ok_or_else(invalid)?;
    let password = body.password;
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash)).await.unwrap();
    if !ok {
        return Err(invalid());
    }
    if verified_at.is_none() {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Confirm your email first with the code we sent you, or register again to get a new code".into(),
        ));
    }
    start_session(&state, id, username).await
}

/// Emails a password reset code when an account uses the email, and sets this browser's `reset`
/// cookie either way, so the response does not reveal which emails are registered. Asking again
/// replaces the code.
async fn forgot(State(state): State<AppState>, Json(body): Json<EmailOnly>) -> ApiResult<impl IntoResponse> {
    let mut conn = state.db.acquire().await?;
    rate_limit_email(&mut conn, body.email.trim()).await?;
    let row: Option<(i64, String, String)> = sqlx::query_as("SELECT id, email, username FROM users WHERE email = ?")
        .bind(body.email.trim())
        .fetch_optional(&mut *conn)
        .await?;
    // Without an account the cookie leads to no code, and every code entered is wrong.
    let mut token = db::random_token();
    if let Some((user_id, email, username)) = row {
        let code;
        (token, code) = create_code(&mut conn, user_id, "reset").await?;
        let body = format!(
            "Hi {username},\n\nYour SET password reset code is:\n\n{code}\n\n\
             Enter it on the page where you asked for it. It expires in 30 minutes.\n\
             If you did not ask for this, ignore this email.\n"
        );
        send_mail(&state, &email, "Reset your SET password", body).await?;
    }
    Ok((AppendHeaders([(SET_COOKIE, auth::code_cookie("reset", &token, CODE_TTL))]), StatusCode::NO_CONTENT))
}

/// Sets a new password when `code` is the one emailed to this browser, logs out every other
/// session, and logs in. The code also proves the email, so it confirms an unconfirmed account.
async fn reset(State(state): State<AppState>, headers: HeaderMap, Json(body): Json<CodeBody>) -> ApiResult<impl IntoResponse> {
    check_password(&body.password)?;
    let user_id = take_code(&state.db, &headers, "reset", &body.code).await?;
    let hash = hash(body.password).await;
    let mut tx = state.db.begin().await?;
    let username: String = sqlx::query_scalar(
        "UPDATE users SET password_hash = ?, email_verified_at = COALESCE(email_verified_at, ?) WHERE id = ? RETURNING username",
    )
    .bind(hash)
    .bind(now())
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((AppendHeaders([(SET_COOKIE, auth::code_cookie("reset", "", 0))]), start_session(&state, user_id, username).await?))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    if let Some(token) = auth::session_token(&headers) {
        sqlx::query("DELETE FROM sessions WHERE token = ?").bind(token).execute(&state.db).await?;
    }
    Ok((AppendHeaders([(SET_COOKIE, auth::clear_cookie())]), StatusCode::NO_CONTENT))
}

async fn me(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Json<Value>> {
    let (email, played, wins): (String, i64, i64) = sqlx::query_as(
        "SELECT email, (SELECT COUNT(*) FROM game_players WHERE user_id = u.id),
                (SELECT COALESCE(SUM(won), 0) FROM game_players WHERE user_id = u.id)
         FROM users u WHERE id = ?",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({ "id": user.id, "username": user.username, "email": email, "played": played, "wins": wins })))
}

async fn players(State(state): State<AppState>, _user: CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, username FROM users WHERE email_verified_at IS NOT NULL ORDER BY username")
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
