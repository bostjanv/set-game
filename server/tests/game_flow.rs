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
    let state = set_server::setup(path.to_str().unwrap(), Mailer::Memory(outbox.clone())).await.unwrap();
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

/// An HTTP client that keeps cookies, like a browser.
fn browser() -> reqwest::Client {
    reqwest::Client::builder().cookie_store(true).build().unwrap()
}

/// The `sid=…` session cookie the response sets, if any.
fn session_cookie(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
        .find(|c| c.starts_with("sid="))
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

    /// The code in the last email sent to `to`.
    fn code(&self, to: &str) -> String {
        let outbox = self.outbox.lock().unwrap();
        let mail = outbox.iter().rev().find(|m| m.to == to).unwrap_or_else(|| panic!("no email to {to}"));
        let code = mail.body.split_whitespace().find(|w| w.len() == 6 && w.chars().all(|c| c.is_ascii_digit()));
        code.unwrap_or_else(|| panic!("no code in {mail:?}")).to_owned()
    }

    /// Registers `name` with the email `name@example.com` and confirms it, which logs in.
    async fn register(&self, name: &str) -> User {
        let http = browser();
        let email = format!("{name}@example.com");
        let resp = self.post(&http, "/api/register", json!({ "email": email, "username": name, "password": "secret1" })).await;
        assert_eq!(resp.status(), 204, "register {name}");
        let resp = self.post(&http, "/api/verify", json!({ "code": self.code(&email) })).await;
        assert_eq!(resp.status(), 200, "verify {name}");
        let cookie = session_cookie(&resp).expect("verify logs in");
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
    let alice_browser = browser();
    let register = |email: &str, name: &str| {
        server.post(&alice_browser, "/api/register", json!({ "email": email, "username": name, "password": "secret1" }))
    };
    let verify = |code: &str| server.post(&alice_browser, "/api/verify", json!({ "code": code }));
    let login = |email: &str, password: &str| server.post(&http, "/api/login", json!({ "email": email, "password": password }));

    assert_eq!(register("not-an-email", "alice").await.status(), 400);
    let resp = register("alice@example.com", "alice").await;
    assert_eq!(resp.status(), 204);
    assert!(session_cookie(&resp).is_none(), "no session before the email is confirmed");
    let first = server.code("alice@example.com");

    // Unconfirmed accounts cannot log in; a wrong password still reads as a wrong password.
    assert_eq!(login("alice@example.com", "secret1").await.status(), 403);
    assert_eq!(login("alice@example.com", "wrong!!").await.status(), 401);

    // The code works only in the browser that registered, so intercepting the email is not enough.
    let resp = server.post(&http, "/api/verify", json!({ "code": first })).await;
    assert_eq!(resp.status(), 400);
    assert!(session_cookie(&resp).is_none());

    // Registering the unconfirmed email again replaces the account, and only the new code works.
    server.age_email_sends(61).await;
    assert_eq!(register("ALICE@example.com", "alice2").await.status(), 204);
    let second = server.code("ALICE@example.com");
    if first != second {
        assert_eq!(verify(&first).await.status(), 400);
    }
    let alice = verify(&second).await;
    assert_eq!(alice.status(), 200);
    assert_eq!(alice.json::<Value>().await.unwrap()["username"], "alice2");
    assert_eq!(verify(&second).await.status(), 400, "codes are single-use");
    assert_eq!(login("alice@example.com", "secret1").await.status(), 200);

    // A confirmed email or username cannot be taken again.
    assert_eq!(register("Alice@Example.com", "someone").await.status(), 409);
    assert_eq!(register("other@example.com", "ALICE2").await.status(), 409);

    // Five wrong guesses use up the code, so it cannot be brute-forced.
    let bob = browser();
    let resp = server.post(&bob, "/api/register", json!({ "email": "bob@example.com", "username": "bob", "password": "secret1" })).await;
    assert_eq!(resp.status(), 204);
    let code: u32 = server.code("bob@example.com").parse().unwrap();
    for i in 1..=5 {
        let wrong = format!("{:06}", (code + i) % 1_000_000);
        assert_eq!(server.post(&bob, "/api/verify", json!({ "code": wrong })).await.status(), 400, "guess {i}");
    }
    let resp = server.post(&bob, "/api/verify", json!({ "code": format!("{code:06}") })).await;
    assert_eq!(resp.status(), 400, "the right code after five wrong ones");

    // Players who have not confirmed their email are not listed.
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
    let http = browser();
    let login = |password: &str| server.post(&http, "/api/login", json!({ "email": "alice@example.com", "password": password }));
    let reset = |code: &str, password: &str| server.post(&http, "/api/reset", json!({ "code": code, "password": password }));

    // Unknown emails get the same answer, cookie included, and no email; any code is then wrong,
    // with the same message as a wrong code for a real account.
    let sent = server.outbox.lock().unwrap().len();
    let resp = server.post(&http, "/api/forgot", json!({ "email": "nobody@example.com" })).await;
    assert_eq!(resp.status(), 204);
    assert!(resp.headers().get("set-cookie").is_some());
    assert_eq!(server.outbox.lock().unwrap().len(), sent);
    let unknown = reset("123456", "newsecret").await;
    assert_eq!(unknown.status(), 400);
    let unknown = unknown.text().await.unwrap();

    assert_eq!(server.post(&http, "/api/forgot", json!({ "email": "Alice@example.com" })).await.status(), 204);
    let code = server.code("alice@example.com");
    let wrong = format!("{:06}", (code.parse::<u32>().unwrap() + 1) % 1_000_000);
    assert_eq!(reset(&wrong, "newsecret").await.text().await.unwrap(), unknown, "wrong codes do not reveal the account");

    // The code works only in the browser that asked for it.
    let other = server.post(&browser(), "/api/reset", json!({ "code": code, "password": "hacked1" })).await;
    assert_eq!(other.status(), 400);

    // A too short password leaves the code usable.
    assert_eq!(reset(&code, "short").await.status(), 400);
    let resp = reset(&code, "newsecret").await;
    assert_eq!(resp.status(), 200);
    assert!(session_cookie(&resp).is_some(), "a reset logs in");

    // The reset logs out the old session, and only the new password works.
    let resp = alice.http.get(format!("http://{}/api/me", server.base)).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(login("secret1").await.status(), 401);
    assert_eq!(login("newsecret").await.status(), 200);
    assert_eq!(reset(&code, "another1").await.status(), 400, "reset codes are single-use");
}

#[tokio::test]
async fn resend_confirmation_code() {
    let server = start_server().await;
    let http = browser();
    let verify = |code: &str| server.post(&http, "/api/verify", json!({ "code": code }));
    let resend = || server.post(&http, "/api/verify/resend", json!({}));

    // Only a browser with a pending registration can ask for a new code.
    assert_eq!(server.post(&browser(), "/api/verify/resend", json!({})).await.status(), 400);
    let resp = server.post(&http, "/api/register", json!({ "email": "bob@example.com", "username": "bob", "password": "secret1" })).await;
    assert_eq!(resp.status(), 204);
    let first = server.code("bob@example.com");
    assert_eq!(resend().await.status(), 429, "one email per minute");

    // Use up the code's guesses; a new code brings new ones and replaces the old code.
    let wrong = format!("{:06}", (first.parse::<u32>().unwrap() + 1) % 1_000_000);
    for _ in 0..5 {
        assert_eq!(verify(&wrong).await.status(), 400);
    }
    server.age_email_sends(61).await;
    assert_eq!(resend().await.status(), 204);
    let second = server.code("bob@example.com");
    if first != second {
        assert_eq!(verify(&first).await.status(), 400);
    }
    let resp = verify(&second).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["username"], "bob");
    assert_eq!(resend().await.status(), 400, "nothing to resend once confirmed");
}

#[tokio::test]
async fn email_rate_limit() {
    let server = start_server().await;
    let http = browser();
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

    // A refused registration leaves the earlier unconfirmed account and its code working.
    assert_eq!(register("bob").await.status(), 204);
    let code = server.code("bob@example.com");
    assert_eq!(register("bobby").await.status(), 429);
    let resp = server.post(&http, "/api/verify", json!({ "code": code })).await;
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
