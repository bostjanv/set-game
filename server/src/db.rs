use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::Connection;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// `path` is a file path such as `set.db`; the file is created when missing.
pub async fn open(path: &str) -> Result<SqlitePool, Error> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    migrate(opts.clone().foreign_keys(false)).await?;
    Ok(SqlitePoolOptions::new().max_connections(5).connect_with(opts).await?)
}

/// Runs the migrations on a connection without foreign keys, so a migration can rebuild a table
/// that others reference: with them on, `DROP TABLE` would cascade and fail the checks. sqlx runs
/// each migration in a transaction, where `PRAGMA foreign_keys` cannot be changed, hence the
/// separate connection. Afterwards every reference must still hold.
async fn migrate(opts: SqliteConnectOptions) -> Result<(), Error> {
    let mut conn = SqliteConnection::connect_with(&opts).await?;
    sqlx::migrate!("./migrations").run(&mut conn).await?;
    let broken: Vec<String> = sqlx::query_scalar(r#"SELECT DISTINCT "table" FROM pragma_foreign_key_check"#)
        .fetch_all(&mut conn)
        .await?;
    conn.close().await?;
    if !broken.is_empty() {
        return Err(format!("migrations left broken foreign keys in tables: {}", broken.join(", ")).into());
    }
    Ok(())
}

pub fn random_token() -> String {
    rand::random::<[u8; 32]>().iter().map(|b| format!("{b:02x}")).collect()
}

pub struct PlayerResult {
    pub user_id: i64,
    pub sets: u32,
    pub won: bool,
}

pub async fn save_game(pool: &SqlitePool, started_at: i64, results: &[PlayerResult]) -> Result<(), Error> {
    let mut tx = pool.begin().await?;
    let game_id = sqlx::query("INSERT INTO games (started_at, finished_at) VALUES (?, ?)")
        .bind(started_at)
        .bind(now())
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
    for r in results {
        sqlx::query("INSERT INTO game_players (game_id, user_id, sets, won) VALUES (?, ?, ?, ?)")
            .bind(game_id)
            .bind(r.user_id)
            .bind(r.sets)
            .bind(r.won)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
