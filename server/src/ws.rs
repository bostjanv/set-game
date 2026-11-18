use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::auth::CurrentUser;
use crate::hub::ClientMsg;
use crate::AppState;

pub async fn handler(ws: WebSocketUpgrade, user: CurrentUser, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| run(socket, user, state))
}

async fn run(socket: WebSocket, user: CurrentUser, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });
    let conn = state.hub.lock().unwrap().connect(user.id, tx.clone());

    while let Some(Ok(frame)) = stream.next().await {
        let Message::Text(text) = frame else { continue };
        let msg = match serde_json::from_str::<ClientMsg>(&text) {
            Ok(msg) => msg,
            Err(e) => {
                tx.send(json!({ "type": "error", "message": format!("Bad message: {e}") }).to_string()).ok();
                continue;
            }
        };
        let invitee_name = match &msg {
            ClientMsg::Invite { user_id } => sqlx::query_scalar::<_, String>("SELECT username FROM users WHERE id = ? AND email_verified_at IS NOT NULL")
                .bind(user_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten(),
            _ => None,
        };
        let mut hub = state.hub.lock().unwrap();
        if let Err(message) = hub.handle(user.id, &user.username, msg, invitee_name) {
            hub.send(user.id, &json!({ "type": "error", "message": message }));
        }
    }

    state.hub.lock().unwrap().disconnect(user.id, conn);
    drop(tx);
    writer.abort();
}
