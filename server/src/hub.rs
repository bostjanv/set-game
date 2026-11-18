//! Live state: connected clients, game rooms, and invitations. Everything lives behind one
//! mutex, so claims are applied strictly in the order the server receives them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::mpsc::UnboundedSender;

use crate::db::{self, PlayerResult};
use crate::rules::{Board, Card, Claim};

pub type UserId = i64;
pub type GameId = u64;

pub const MAX_PLAYERS: usize = 8;
pub const LOCKOUT: Duration = Duration::from_secs(3);
/// A waiting game whose leader has been offline this long is cancelled.
const LEADER_GRACE: Duration = Duration::from_secs(60);
/// A running game with no connected player for this long is abandoned and not recorded.
const ABANDON_AFTER: Duration = Duration::from_secs(600);
/// Finished games stay viewable for this long, then are removed.
const KEEP_FINISHED: Duration = Duration::from_secs(600);

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    CreateGame,
    Invite { user_id: UserId },
    RespondInvite { game_id: GameId, accept: bool },
    StartGame,
    LeaveGame,
    Claim { cards: [Card; 3] },
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum Status {
    Waiting,
    Playing,
    Finished,
}

struct Player {
    id: UserId,
    username: String,
    score: u32,
    locked_until: Option<Instant>,
    left: bool,
}

struct Game {
    leader: UserId,
    status: Status,
    players: Vec<Player>,
    invited: Vec<(UserId, String)>,
    board: Option<Board>,
    started_at: i64,
    last_claim: Option<Value>,
    claim_seq: u64,
    winners: Vec<UserId>,
    /// When the sweeper first saw the game idle (see the constants above).
    idle_since: Option<Instant>,
}

impl Game {
    fn player_mut(&mut self, id: UserId) -> Option<&mut Player> {
        self.players.iter_mut().find(|p| p.id == id)
    }
}

pub struct Hub {
    db: SqlitePool,
    conns: HashMap<UserId, Vec<(u64, UnboundedSender<String>)>>,
    games: HashMap<GameId, Game>,
    /// The game each user is currently in (waiting, playing, or viewing results).
    user_game: HashMap<UserId, GameId>,
    next_id: u64,
}

type HubResult = Result<(), String>;

impl Hub {
    pub fn new(db: SqlitePool) -> Self {
        Hub { db, conns: HashMap::new(), games: HashMap::new(), user_game: HashMap::new(), next_id: 1 }
    }

    pub fn connect(&mut self, user: UserId, tx: UnboundedSender<String>) -> u64 {
        let conn = self.next_id;
        self.next_id += 1;
        let first = !self.conns.contains_key(&user);
        self.conns.entry(user).or_default().push((conn, tx));
        if first {
            self.broadcast_online();
            if let Some(&gid) = self.user_game.get(&user) {
                self.sync_game(gid);
            }
        } else {
            self.send(user, &self.online_msg());
        }
        self.send_invites(user);
        let game = self.user_game.get(&user).map(|&gid| self.game_view(gid)).unwrap_or(Value::Null);
        self.send(user, &json!({ "type": "game", "game": game }));
        conn
    }

    pub fn disconnect(&mut self, user: UserId, conn: u64) {
        let Some(list) = self.conns.get_mut(&user) else { return };
        list.retain(|(id, _)| *id != conn);
        if list.is_empty() {
            self.conns.remove(&user);
            self.broadcast_online();
            if let Some(&gid) = self.user_game.get(&user) {
                self.sync_game(gid);
            }
        }
    }

    pub fn is_online(&self, user: UserId) -> bool {
        self.conns.contains_key(&user)
    }

    /// Applies a client message. `invitee_name` is the username of `Invite`'s target,
    /// looked up by the caller; it is None when that user does not exist.
    pub fn handle(&mut self, user: UserId, username: &str, msg: ClientMsg, invitee_name: Option<String>) -> HubResult {
        match msg {
            ClientMsg::CreateGame => self.create_game(user, username),
            ClientMsg::Invite { user_id } => {
                let name = invitee_name.ok_or("No such player")?;
                self.invite(user, user_id, name)
            }
            ClientMsg::RespondInvite { game_id, accept } => self.respond(user, username, game_id, accept),
            ClientMsg::StartGame => self.start(user),
            ClientMsg::LeaveGame => self.leave(user),
            ClientMsg::Claim { cards } => self.claim(user, cards),
        }
    }

    /// The user's game unless it has finished: a player can be in one active game at a time.
    fn active_game(&self, user: UserId) -> Option<GameId> {
        self.user_game.get(&user).copied().filter(|gid| self.games[gid].status != Status::Finished)
    }

    fn create_game(&mut self, user: UserId, username: &str) -> HubResult {
        if self.active_game(user).is_some() {
            return Err("You are already in a game".into());
        }
        self.leave(user).ok(); // stop viewing a finished game
        let gid = self.next_id;
        self.next_id += 1;
        self.games.insert(
            gid,
            Game {
                leader: user,
                status: Status::Waiting,
                players: vec![Player { id: user, username: username.into(), score: 0, locked_until: None, left: false }],
                invited: Vec::new(),
                board: None,
                started_at: 0,
                last_claim: None,
                claim_seq: 0,
                winners: Vec::new(),
                idle_since: None,
            },
        );
        self.user_game.insert(user, gid);
        self.sync_game(gid);
        Ok(())
    }

    fn led_waiting_game(&mut self, user: UserId) -> Result<GameId, String> {
        let gid = self.active_game(user).ok_or("You are not in a game")?;
        let game = &self.games[&gid];
        if game.leader != user {
            return Err("Only the leader can do that".into());
        }
        if game.status != Status::Waiting {
            return Err("The game has already started".into());
        }
        Ok(gid)
    }

    fn invite(&mut self, user: UserId, target: UserId, target_name: String) -> HubResult {
        let gid = self.led_waiting_game(user)?;
        let game = self.games.get_mut(&gid).unwrap();
        if game.players.iter().any(|p| p.id == target) || game.invited.iter().any(|(id, _)| *id == target) {
            return Err(format!("{target_name} is already in or invited to this game"));
        }
        if game.players.len() + game.invited.len() >= MAX_PLAYERS {
            return Err(format!("A game has at most {MAX_PLAYERS} players"));
        }
        game.invited.push((target, target_name));
        self.sync_game(gid);
        self.send_invites(target);
        Ok(())
    }

    fn respond(&mut self, user: UserId, username: &str, gid: GameId, accept: bool) -> HubResult {
        let invited = self.games.get(&gid).is_some_and(|g| g.invited.iter().any(|(id, _)| *id == user));
        if !invited {
            self.send_invites(user);
            return Err("That invitation is no longer valid".into());
        }
        if accept && self.active_game(user).is_some() {
            return Err("Leave your current game before joining another".into());
        }
        if accept {
            self.leave(user).ok(); // stop viewing a finished game
        }
        let game = self.games.get_mut(&gid).unwrap();
        game.invited.retain(|(id, _)| *id != user);
        if accept {
            game.players.push(Player { id: user, username: username.into(), score: 0, locked_until: None, left: false });
            self.user_game.insert(user, gid);
        }
        self.sync_game(gid);
        self.send_invites(user);
        Ok(())
    }

    fn start(&mut self, user: UserId) -> HubResult {
        let gid = self.led_waiting_game(user)?;
        let game = self.games.get_mut(&gid).unwrap();
        game.status = Status::Playing;
        game.board = Some(Board::new(&mut rand::thread_rng()));
        game.started_at = db::now();
        let expired: Vec<UserId> = game.invited.drain(..).map(|(id, _)| id).collect();
        for id in expired {
            self.send_invites(id);
        }
        self.sync_game(gid);
        Ok(())
    }

    fn leave(&mut self, user: UserId) -> HubResult {
        let gid = self.user_game.remove(&user).ok_or("You are not in a game")?;
        self.send(user, &json!({ "type": "game", "game": null }));
        let game = self.games.get_mut(&gid).unwrap();
        match game.status {
            Status::Waiting if game.leader == user => {
                self.close_game(gid, Some("The leader closed the game"));
                return Ok(());
            }
            Status::Waiting => game.players.retain(|p| p.id != user),
            // A player who leaves a running game keeps their score in the results.
            Status::Playing => {
                game.player_mut(user).unwrap().left = true;
                if game.players.iter().all(|p| p.left) {
                    self.close_game(gid, None);
                    return Ok(());
                }
            }
            Status::Finished => {}
        }
        if !self.user_game.values().any(|&g| g == gid) {
            self.close_game(gid, None);
        } else {
            self.sync_game(gid);
        }
        Ok(())
    }

    /// Removes a game without recording it, telling remaining players and invitees.
    fn close_game(&mut self, gid: GameId, notice: Option<&str>) {
        let Some(game) = self.games.remove(&gid) else { return };
        for p in &game.players {
            if self.user_game.get(&p.id) == Some(&gid) {
                self.user_game.remove(&p.id);
                self.send(p.id, &json!({ "type": "game", "game": null }));
                if let Some(text) = notice {
                    self.send(p.id, &json!({ "type": "notice", "message": text }));
                }
            }
        }
        for (id, _) in &game.invited {
            self.send_invites(*id);
        }
    }

    fn claim(&mut self, user: UserId, cards: [Card; 3]) -> HubResult {
        let gid = self.active_game(user).ok_or("You are not in a game")?;
        let game = self.games.get_mut(&gid).unwrap();
        if game.status != Status::Playing {
            return Err("The game is not running".into());
        }
        let now = Instant::now();
        let player = game.player_mut(user).unwrap();
        if player.locked_until.is_some_and(|t| t > now) {
            self.send(user, &json!({ "type": "claim_result", "result": "locked" }));
            return Ok(());
        }
        let board = game.board.as_mut().unwrap();
        let result = board.claim(cards);
        let player = game.player_mut(user).unwrap();
        let label = match result {
            Claim::Ok => {
                player.score += 1;
                game.claim_seq += 1;
                game.last_claim = Some(json!({ "by": user, "cards": cards, "seq": game.claim_seq }));
                if game.board.as_ref().unwrap().is_over() {
                    self.finish(gid);
                }
                "ok"
            }
            Claim::NotASet => {
                player.locked_until = Some(now + LOCKOUT);
                "not_a_set"
            }
            Claim::Stale => "stale",
        };
        self.send(user, &json!({ "type": "claim_result", "result": label }));
        if result != Claim::Stale {
            self.sync_game(gid);
        }
        Ok(())
    }

    fn finish(&mut self, gid: GameId) {
        let game = self.games.get_mut(&gid).unwrap();
        game.status = Status::Finished;
        game.idle_since = Some(Instant::now());
        let best = game.players.iter().map(|p| p.score).max().unwrap_or(0);
        game.winners = game.players.iter().filter(|p| p.score == best).map(|p| p.id).collect();
        // Solo games are practice: they count neither as played nor as won.
        if game.players.len() < 2 {
            return;
        }
        let results: Vec<PlayerResult> = game
            .players
            .iter()
            .map(|p| PlayerResult { user_id: p.id, sets: p.score, won: p.score == best })
            .collect();
        let (db, started_at) = (self.db.clone(), game.started_at);
        tokio::spawn(async move {
            if let Err(e) = db::save_game(&db, started_at, &results).await {
                tracing::error!("saving game result failed: {e}");
            }
        });
    }

    /// Cancels waiting games whose leader went away, abandons running games nobody is
    /// connected to, and removes old finished games. Run periodically.
    pub fn sweep(&mut self) {
        let now = Instant::now();
        let mut expired = Vec::new();
        for (&gid, game) in self.games.iter_mut() {
            let (idle, limit) = match game.status {
                Status::Waiting => (!self.conns.contains_key(&game.leader), LEADER_GRACE),
                Status::Playing => (game.players.iter().all(|p| p.left || !self.conns.contains_key(&p.id)), ABANDON_AFTER),
                Status::Finished => (true, KEEP_FINISHED),
            };
            if !idle {
                game.idle_since = None;
            } else if game.idle_since.get_or_insert(now).elapsed() >= limit {
                expired.push(gid);
            }
        }
        for gid in expired {
            self.close_game(gid, Some("The game was closed"));
        }
    }

    fn game_view(&self, gid: GameId) -> Value {
        let game = &self.games[&gid];
        let now = Instant::now();
        let players: Vec<Value> = game
            .players
            .iter()
            .map(|p| {
                let locked_ms = p.locked_until.map_or(0, |t| t.saturating_duration_since(now).as_millis() as u64);
                json!({
                    "id": p.id,
                    "username": p.username,
                    "score": p.score,
                    "connected": self.conns.contains_key(&p.id),
                    "left": p.left,
                    "locked_ms": locked_ms,
                })
            })
            .collect();
        let invited: Vec<Value> = game.invited.iter().map(|(id, name)| json!({ "id": id, "username": name })).collect();
        json!({
            "id": gid,
            "leader": game.leader,
            "status": game.status,
            "players": players,
            "invited": invited,
            "table": game.board.as_ref().map_or(&[][..], |b| b.table()),
            "deck_remaining": game.board.as_ref().map_or(0, |b| b.deck_remaining()),
            "last_claim": game.last_claim,
            "winners": game.winners,
        })
    }

    fn sync_game(&self, gid: GameId) {
        let msg = json!({ "type": "game", "game": self.game_view(gid) });
        for p in &self.games[&gid].players {
            if self.user_game.get(&p.id) == Some(&gid) {
                self.send(p.id, &msg);
            }
        }
    }

    fn send_invites(&self, user: UserId) {
        let invites: Vec<Value> = self
            .games
            .iter()
            .filter(|(_, g)| g.invited.iter().any(|(id, _)| *id == user))
            .map(|(gid, g)| {
                let leader = g.players.iter().find(|p| p.id == g.leader).map_or("", |p| p.username.as_str());
                json!({ "game_id": gid, "leader": leader })
            })
            .collect();
        self.send(user, &json!({ "type": "invites", "invites": invites }));
    }

    fn online_msg(&self) -> Value {
        json!({ "type": "online", "user_ids": self.conns.keys().collect::<Vec<_>>() })
    }

    fn broadcast_online(&self) {
        let text = self.online_msg().to_string();
        for list in self.conns.values() {
            for (_, tx) in list {
                tx.send(text.clone()).ok();
            }
        }
    }

    pub fn send(&self, user: UserId, msg: &Value) {
        if let Some(list) = self.conns.get(&user) {
            let text = msg.to_string();
            for (_, tx) in list {
                tx.send(text.clone()).ok();
            }
        }
    }
}
