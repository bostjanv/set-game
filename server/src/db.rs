use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

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
    let pool = SqlitePoolOptions::new().max_connections(5).connect_with(opts).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
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
