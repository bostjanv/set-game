use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::header::COOKIE;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use axum::http::StatusCode;

use crate::api::ApiError;
use crate::AppState;

pub const COOKIE_NAME: &str = "sid";
/// Holds the secret of a pending email confirmation, so only the registering browser can enter its code.
pub const VERIFY_COOKIE: &str = "verify";

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>()).expect("16 bytes is a valid salt");
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing with default params")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub fn session_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000")
}

pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

pub fn verify_cookie(token: &str, max_age: i64) -> String {
    format!("{VERIFY_COOKIE}={token}; Path=/api; HttpOnly; SameSite=Strict; Max-Age={max_age}")
}

pub fn session_token(headers: &HeaderMap) -> Option<String> {
    cookie(headers, COOKIE_NAME)
}

pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(name)?.strip_prefix('=').map(str::to_owned))
        .filter(|t| !t.is_empty())
}

/// The logged-in user, from the session cookie. Requests without a valid session get 401.
#[derive(Clone, Debug)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let unauthorized = || ApiError(StatusCode::UNAUTHORIZED, "Not logged in".into());
        let token = session_token(&parts.headers).ok_or_else(unauthorized)?;
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT u.id, u.username FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token = ?",
        )
        .bind(token)
        .fetch_optional(&state.db)
        .await?;
        let (id, username) = row.ok_or_else(unauthorized)?;
        Ok(CurrentUser { id, username })
    }
}
