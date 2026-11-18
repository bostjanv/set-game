use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use set_server::mail::{Mail, Mailer};
use set_server::rules::find_set;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

struct Server {
    base: String,
    outbox: Arc<Mutex<Vec<Mail>>>,
    db: sqlx::SqlitePool,
}

async fn start_server() -> Server {
    let path = std::env::temp_dir().join(format!("set-test-{}-{}.db", std::process::id(), rand_suffix()));
    for ext in ["", "-wal", "-shm"] {
        std::fs::remove_file(format!("{}{ext}", path.display())).ok();
    }
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let state = set_server::setup(path.to_str().unwrap(), Mailer::Memory(outbox.clone()), "http://set.test/").await.unwrap();
    let db = state.db.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, set_server::router(state, None)).await.unwrap() });
    Server { base, outbox, db }
}

/// Unique per test in this process, so parallel tests never share a database file.
fn rand_suffix() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

struct User {
    http: reqwest::Client,
    cookie: String,
    id: i64,
}

impl Server {
    async fn post(&self, http: &reqwest::Client, path: &str, body: Value) -> reqwest::Response {
        http.post(format!("http://{}{path}", self.base)).json(&body).send().await.unwrap()
    }

    /// The token in the link of the last email sent to `to` that leads to `path`.
    fn link_token(&self, to: &str, path: &str) -> String {
        let outbox = self.outbox.lock().unwrap();
        let mail = outbox.iter().rev().find(|m| m.to == to).unwrap_or_else(|| panic!("no email to {to}"));
        let prefix = format!("http://set.test{path}?token=");
        let start = mail.body.find(&prefix).unwrap_or_else(|| panic!("no {path} link in {mail:?}")) + prefix.len();
        mail.body[start..].split_whitespace().next().unwrap().to_owned()
    }

    /// Registers `name` with the email `name@example.com` and confirms it, which logs in.
    async fn register(&self, name: &str) -> User {
        let http = reqwest::Client::builder().cookie_store(true).build().unwrap();
        let email = format!("{name}@example.com");
        let resp = self.post(&http, "/api/register", json!({ "email": email, "username": name, "password": "secret1" })).await;
        assert_eq!(resp.status(), 204, "register {name}");
        let resp = self.post(&http, "/api/verify", json!({ "token": self.link_token(&email, "/verify") })).await;
        assert_eq!(resp.status(), 200, "verify {name}");
        let cookie = resp.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_owned();
        let body: Value = resp.json().await.unwrap();
        User { http, cookie, id: body["id"].as_i64().unwrap() }
    }

    /// Moves the recorded email requests back in time, as if `secs` had passed.
    async fn age_email_sends(&self, secs: i64) {
        sqlx::query("UPDATE email_sends SET sent_at = sent_at - ?").bind(secs).execute(&self.db).await.unwrap();
    }

    async fn get(&self, user: &User, path: &str) -> Value {
        user.http.get(format!("http://{}{path}", self.base)).send().await.unwrap().json().await.unwrap()
    }

    async fn connect(&self, user: &User) -> Ws {
        let mut req = format!("ws://{}/ws", self.base).into_client_request().unwrap();
        req.headers_mut().insert("cookie", user.cookie.parse().unwrap());
        let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        Ws { ws, game: Value::Null }
    }
}

struct Ws {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// The latest game state received.
    game: Value,
}

impl Ws {
    async fn send(&mut self, msg: Value) {
        self.ws.send(Message::Text(msg.to_string().into())).await.unwrap();
    }

    /// Reads messages until one matches, remembering game updates on the way.
    async fn until(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let Some(Ok(Message::Text(text))) = self.ws.next().await else { panic!("socket closed") };
                let v: Value = serde_json::from_str(&text).unwrap();
                if v["type"] == "game" {
                    self.game = v["game"].clone();
                }
                if pred(&v) {
                    return v;
                }
            }
        })
        .await
        .expect("timed out waiting for message")
    }

    async fn until_type(&mut self, ty: &str) -> Value {
        self.until(|v| v["type"] == ty).await
    }

    /// Returns the latest game state once it matches, which may already be the case.
    async fn until_game(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        if !self.game.is_null() && pred(&self.game) {
            return self.game.clone();
        }
        self.until(|v| v["type"] == "game" && pred(&v["game"])).await["game"].clone()
    }
}

fn table(game: &Value) -> Vec<u8> {
    game["table"].as_array().unwrap().iter().map(|c| c.as_u64().unwrap() as u8).collect()
}

fn score(game: &Value, id: i64) -> u64 {
    game["players"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap()["score"].as_u64().unwrap()
}

#[tokio::test]
async fn email_registration() {
    let server = start_server().await;
    let http = reqwest::Client::new();
    let register = |email: &str, name: &str| {
        server.post(&http, "/api/register", json!({ "email": email, "username": name, "password": "secret1" }))
    };
    let login = |email: &str, password: &str| server.post(&http, "/api/login", json!({ "email": email, "password": password }));

    assert_eq!(register("not-an-email", "alice").await.status(), 400);
    let resp = register("alice@example.com", "alice").await;
    assert_eq!(resp.status(), 204);
    assert!(resp.headers().get("set-cookie").is_none(), "no session before the email is confirmed");
    let first = server.link_token("alice@example.com", "/verify");

    // Unconfirmed accounts cannot log in; a wrong password still reads as a wrong password.
    assert_eq!(login("alice@example.com", "secret1").await.status(), 403);
    assert_eq!(login("alice@example.com", "wrong!!").await.status(), 401);

    // Registering the unconfirmed email again replaces the account, and only the new link works.
    server.age_email_sends(61).await;
    assert_eq!(register("ALICE@example.com", "alice2").await.status(), 204);
    let resp = server.post(&http, "/api/verify", json!({ "token": first })).await;
    assert_eq!(resp.status(), 400);
    let alice = server.post(&http, "/api/verify", json!({ "token": server.link_token("ALICE@example.com", "/verify") })).await;
    assert_eq!(alice.status(), 200);
    assert_eq!(alice.json::<Value>().await.unwrap()["username"], "alice2");
    assert_eq!(login("alice@example.com", "secret1").await.status(), 200);

    // A confirmed email or username cannot be taken again.
    assert_eq!(register("Alice@Example.com", "someone").await.status(), 409);
    assert_eq!(register("other@example.com", "ALICE2").await.status(), 409);

    // Players who have not confirmed their email are not listed.
    assert_eq!(register("bob@example.com", "bob").await.status(), 204);
    let carol = server.register("carol").await;
    let players = server.get(&carol, "/api/players").await;
    assert_eq!(players.as_array().unwrap().iter().map(|p| p["username"].clone()).collect::<Vec<_>>(), [json!("alice2"), json!("carol")]);
    assert_eq!(server.get(&carol, "/api/me").await["email"], "carol@example.com");

    // Unauthenticated requests are refused.
    let resp = reqwest::get(format!("http://{}/api/players", server.base)).await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn password_reset() {
    let server = start_server().await;
    let alice = server.register("alice").await;
    server.age_email_sends(61).await;
    let http = reqwest::Client::new();
    let login = |password: &str| server.post(&http, "/api/login", json!({ "email": "alice@example.com", "password": password }));

    // Unknown emails get the same answer and no email.
    let sent = server.outbox.lock().unwrap().len();
    assert_eq!(server.post(&http, "/api/forgot", json!({ "email": "nobody@example.com" })).await.status(), 204);
    assert_eq!(server.outbox.lock().unwrap().len(), sent);

    assert_eq!(server.post(&http, "/api/forgot", json!({ "email": "Alice@example.com" })).await.status(), 204);
    let token = server.link_token("alice@example.com", "/reset");
    // A confirmation link is not a reset link, and a too short password leaves the token usable.
    let verify = server.post(&http, "/api/verify", json!({ "token": token })).await;
    assert_eq!(verify.status(), 400);
    let short = server.post(&http, "/api/reset", json!({ "token": token, "password": "short" })).await;
    assert_eq!(short.status(), 400);
    let resp = server.post(&http, "/api/reset", json!({ "token": token, "password": "newsecret" })).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("set-cookie").is_some(), "a reset logs in");

    // The reset logs out the old session, and only the new password works.
    let resp = alice.http.get(format!("http://{}/api/me", server.base)).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(login("secret1").await.status(), 401);
    assert_eq!(login("newsecret").await.status(), 200);
    let again = server.post(&http, "/api/reset", json!({ "token": token, "password": "another1" })).await;
    assert_eq!(again.status(), 400, "reset links are single-use");
}

#[tokio::test]
async fn email_rate_limit() {
    let server = start_server().await;
    let http = reqwest::Client::new();
    let forgot = |email: &str| server.post(&http, "/api/forgot", json!({ "email": email }));
    let register = |name: &str| {
        server.post(&http, "/api/register", json!({ "email": "bob@example.com", "username": name, "password": "secret1" }))
    };
    server.register("alice").await;
    server.age_email_sends(61).await;

    // One email per minute: the answer is the same for registered and unknown addresses.
    for email in ["alice@example.com", "nobody@example.com"] {
        assert_eq!(forgot(email).await.status(), 204);
        assert_eq!(forgot(&email.to_uppercase()).await.status(), 429, "{email}");
    }
    assert_eq!(server.outbox.lock().unwrap().iter().filter(|m| m.subject.contains("Reset")).count(), 1);

    // A refused registration leaves the earlier unconfirmed account and its link working.
    assert_eq!(register("bob").await.status(), 204);
    let token = server.link_token("bob@example.com", "/verify");
    assert_eq!(register("bobby").await.status(), 429);
    let resp = server.post(&http, "/api/verify", json!({ "token": token })).await;
    assert_eq!(resp.json::<Value>().await.unwrap()["username"], "bob");

    // Five emails per hour, counting only the last hour.
    for _ in 0..3 {
        server.age_email_sends(61).await;
        assert_eq!(forgot("alice@example.com").await.status(), 204);
    }
    server.age_email_sends(61).await;
    assert_eq!(forgot("alice@example.com").await.status(), 429, "sixth email within an hour");
    server.age_email_sends(3600 - 5 * 61).await;
    assert_eq!(forgot("alice@example.com").await.status(), 204, "the first email is over an hour old");
}

#[tokio::test]
async fn full_game() {
    let server = start_server().await;
    let alice = server.register("alice").await;
    let bob = server.register("bob").await;
    let carol = server.register("carol").await;
    let dave = server.register("dave").await;

    let mut a = server.connect(&alice).await;
    let mut b = server.connect(&bob).await;
    let mut c = server.connect(&carol).await;
    let mut d = server.connect(&dave).await;

    a.send(json!({ "type": "create_game" })).await;
    let game = a.until_game(|g| g["status"] == "waiting").await;
    let game_id = game["id"].clone();
    for id in [bob.id, carol.id, dave.id] {
        a.send(json!({ "type": "invite", "user_id": id })).await;
    }
    a.until_game(|g| g["invited"].as_array().unwrap().len() == 3).await;

    // Bob accepts, carol declines, dave never answers.
    b.until(|v| v["type"] == "invites" && v["invites"][0]["leader"] == "alice").await;
    b.send(json!({ "type": "respond_invite", "game_id": game_id, "accept": true })).await;
    b.until_game(|g| g["players"].as_array().unwrap().len() == 2).await;
    c.until(|v| v["type"] == "invites" && v["invites"].as_array().unwrap().len() == 1).await;
    c.send(json!({ "type": "respond_invite", "game_id": game_id, "accept": false })).await;
    c.until(|v| v["type"] == "invites" && v["invites"].as_array().unwrap().is_empty()).await;

    // Only the leader can start.
    b.send(json!({ "type": "start_game" })).await;
    assert_eq!(b.until_type("error").await["message"], "Only the leader can do that");

    a.send(json!({ "type": "start_game" })).await;
    let game = a.until_game(|g| g["status"] == "playing").await;
    assert_eq!(table(&game).len() % 3, 0);
    assert!(table(&game).len() >= 12);
    assert_eq!(game["invited"], json!([]));

    // After the start nobody can join: dave's pending invite is gone and invites are refused.
    d.until(|v| v["type"] == "invites" && v["invites"].as_array().unwrap().is_empty()).await;
    d.send(json!({ "type": "respond_invite", "game_id": game_id, "accept": true })).await;
    assert_eq!(d.until_type("error").await["message"], "That invitation is no longer valid");
    a.send(json!({ "type": "invite", "user_id": carol.id })).await;
    assert_eq!(a.until_type("error").await["message"], "The game has already started");

    // Both players claim the same set at once: exactly one gets it, the other is not penalized.
    b.until_game(|g| g["status"] == "playing").await;
    let set = find_set(&table(&a.game)).unwrap();
    a.send(json!({ "type": "claim", "cards": set })).await;
    b.send(json!({ "type": "claim", "cards": set })).await;
    let ra = a.until_type("claim_result").await["result"].clone();
    let rb = b.until_type("claim_result").await["result"].clone();
    let mut results = [ra.as_str().unwrap(), rb.as_str().unwrap()];
    results.sort();
    assert_eq!(results, ["ok", "stale"]);
    let game = a.until_game(|g| g["last_claim"]["seq"] == 1).await;
    assert_eq!(score(&game, alice.id) + score(&game, bob.id), 1);

    // A wrong claim locks bob out for 10 seconds, even for a valid set.
    b.until_game(|g| g["last_claim"]["seq"] == 1).await;
    let t = table(&b.game);
    let wrong = (2..t.len()).find(|&k| !set_server::rules::is_set(t[0], t[1], t[k])).unwrap();
    b.send(json!({ "type": "claim", "cards": [t[0], t[1], t[wrong]] })).await;
    assert_eq!(b.until_type("claim_result").await["result"], "not_a_set");
    let locked = b.until_game(|g| g["players"].as_array().unwrap().iter().any(|p| p["locked_ms"].as_u64().unwrap() > 0)).await;
    assert_eq!(table(&locked), t, "a wrong claim leaves the table alone");
    let set = find_set(&t).unwrap();
    b.send(json!({ "type": "claim", "cards": set })).await;
    assert_eq!(b.until_type("claim_result").await["result"], "locked");

    // Bob reconnects mid-game and gets the same game with his score.
    let bob_score = score(&b.game, bob.id);
    drop(b);
    a.until_game(|g| g["players"].as_array().unwrap().iter().any(|p| p["id"] == bob.id && p["connected"] == false)).await;
    let mut b = server.connect(&bob).await;
    let game = b.until_game(|g| g["status"] == "playing").await;
    assert_eq!(score(&game, bob.id), bob_score);

    // Alice finds every remaining set, so she wins.
    loop {
        let game = a.game.clone();
        if game["status"] == "finished" {
            break;
        }
        let seq = game["last_claim"]["seq"].as_u64().unwrap();
        let set = find_set(&table(&game)).expect("a running game always has a set on the table");
        a.send(json!({ "type": "claim", "cards": set })).await;
        assert_eq!(a.until_type("claim_result").await["result"], "ok");
        a.until_game(|g| g["last_claim"]["seq"] == seq + 1).await;
    }
    let game = a.game.clone();
    assert_eq!(game["winners"], json!([alice.id]));
    assert_eq!(game["deck_remaining"], 0);
    assert_eq!(score(&game, alice.id) + score(&game, bob.id), (81 - table(&game).len() as u64) / 3);
    b.until_game(|g| g["status"] == "finished").await;

    // The result is recorded.
    let mut board = Value::Null;
    for _ in 0..50 {
        board = server.get(&alice, "/api/leaderboard").await;
        if board.as_array().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(board, json!([
        { "username": "alice", "played": 1, "wins": 1 },
        { "username": "bob", "played": 1, "wins": 0 },
    ]));
    let me = server.get(&bob, "/api/me").await;
    assert_eq!((me["played"].clone(), me["wins"].clone()), (json!(1), json!(0)));

    // Leaving the finished game frees alice to start a new one.
    a.send(json!({ "type": "leave_game" })).await;
    a.until(|v| v["type"] == "game" && v["game"].is_null()).await;
    a.send(json!({ "type": "create_game" })).await;
    a.until_game(|g| g["status"] == "waiting" && g["id"] != game_id).await;
}

#[tokio::test]
async fn solo_game_is_not_recorded() {
    let server = start_server().await;
    let alice = server.register("alice").await;
    let mut a = server.connect(&alice).await;
    a.send(json!({ "type": "create_game" })).await;
    a.until_game(|g| g["status"] == "waiting").await;
    a.send(json!({ "type": "start_game" })).await;
    a.until_game(|g| g["status"] == "playing").await;
    while a.game["status"] != "finished" {
        let set = find_set(&table(&a.game)).unwrap();
        a.send(json!({ "type": "claim", "cards": set })).await;
        a.until_type("claim_result").await;
        a.until_type("game").await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(server.get(&alice, "/api/leaderboard").await, json!([]));
}

#[tokio::test]
async fn leader_leaving_cancels_waiting_game() {
    let server = start_server().await;
    let alice = server.register("alice").await;
    let bob = server.register("bob").await;
    let mut a = server.connect(&alice).await;
    let mut b = server.connect(&bob).await;
    a.send(json!({ "type": "create_game" })).await;
    let game_id = a.until_game(|g| g["status"] == "waiting").await["id"].clone();
    a.send(json!({ "type": "invite", "user_id": bob.id })).await;
    b.until(|v| v["type"] == "invites" && !v["invites"].as_array().unwrap().is_empty()).await;
    b.send(json!({ "type": "respond_invite", "game_id": game_id, "accept": true })).await;
    b.until_game(|g| g["players"].as_array().unwrap().len() == 2).await;
    a.send(json!({ "type": "leave_game" })).await;
    b.until(|v| v["type"] == "game" && v["game"].is_null()).await;
    assert_eq!(b.until_type("notice").await["message"], "The leader closed the game");
}
