export interface Me {
  id: number;
  username: string;
  email: string;
  played: number;
  wins: number;
}

export interface PlayerView {
  id: number;
  username: string;
  score: number;
  connected: boolean;
  left: boolean;
  locked_ms: number;
}

export interface GameView {
  id: number;
  leader: number;
  status: 'waiting' | 'playing' | 'finished';
  players: PlayerView[];
  invited: { id: number; username: string }[];
  table: number[];
  deck_remaining: number;
  last_claim: { by: number; cards: number[]; seq: number } | null;
  winners: number[];
}

export interface GameInvite {
  game_id: number;
  leader: string;
}

export type ServerMsg =
  | { type: 'online'; user_ids: number[] }
  | { type: 'invites'; invites: GameInvite[] }
  | { type: 'game'; game: GameView | null }
  | { type: 'claim_result'; result: 'ok' | 'not_a_set' | 'stale' | 'locked' }
  | { type: 'error'; message: string }
  | { type: 'notice'; message: string };

export type ClientMsg =
  | { type: 'create_game' }
  | { type: 'invite'; user_id: number }
  | { type: 'respond_invite'; game_id: number; accept: boolean }
  | { type: 'start_game' }
  | { type: 'leave_game' }
  | { type: 'claim'; cards: number[] };

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

/**
 * The path prefix the app is served under, such as "/set-game" behind a reverse proxy, or "" at the root.
 * The script is served from `<prefix>/assets/` (or `/src/` in development), so drop its last two segments.
 */
export const BASE = new URL(import.meta.url).pathname.replace(/\/[^/]*\/[^/]*$/, '');

export async function api<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(BASE + path, {
    method,
    headers: body === undefined ? {} : { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (res.status === 204) return undefined as T;
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new ApiError(res.status, data.error ?? res.statusText);
  return data as T;
}

/** A WebSocket that reconnects after drops until closed. */
export class Socket {
  private ws: WebSocket | null = null;
  private closed = false;

  constructor(private onMessage: (msg: ServerMsg) => void, private onStatus: (connected: boolean) => void) {
    this.open();
  }

  private open() {
    const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}${BASE}/ws`;
    const ws = new WebSocket(url);
    this.ws = ws;
    ws.onopen = () => this.onStatus(true);
    ws.onmessage = (e) => this.onMessage(JSON.parse(e.data));
    ws.onclose = () => {
      this.onStatus(false);
      setTimeout(() => this.closed || this.open(), 1000);
    };
  }

  send(msg: ClientMsg) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
  }

  close() {
    this.closed = true;
    this.ws?.close();
  }
}
