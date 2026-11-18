import './style.css';
import { api, ApiError, GameInvite, GameView, Me, ServerMsg, Socket } from './api';
import { Board } from './board';
import { h, toast } from './dom';

/** Colors that identify players on the board and in the score list. */
const PLAYER_COLORS = [0x2563eb, 0xea580c, 0x0d9488, 0xc026d3, 0x65a30d, 0xdc2626, 0x7c3aed, 0xca8a04];
const hex = (c: number) => `#${c.toString(16).padStart(6, '0')}`;

const app = document.getElementById('app')!;

const state = {
  me: null as Me | null,
  socket: null as Socket | null,
  connected: false,
  online: new Set<number>(),
  invites: [] as GameInvite[],
  game: null as GameView | null,
  players: [] as { id: number; username: string }[],
  leaderboard: [] as { username: string; played: number; wins: number }[],
};

let board: Board | null = null;
let boardGameId: number | null = null;
let lockTimer: number | undefined;

function navigate(path: string) {
  if (location.pathname + location.search !== path) history.pushState(null, '', path);
  render();
}
window.addEventListener('popstate', () => render());

function link(path: string, label: string) {
  return h('a', { href: path, onclick: (e: Event) => (e.preventDefault(), navigate(path)) }, label);
}

async function boot() {
  // A confirmation link logs in as its account, replacing any current session.
  if (location.pathname !== '/verify') {
    try {
      state.me = await api<Me>('GET', '/api/me');
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 401)) throw e;
    }
  }
  if (state.me) startSession();
  render();
}

function startSession() {
  state.socket = new Socket(onMessage, (connected) => {
    state.connected = connected;
    renderStatus();
    if (!connected) checkSession();
  });
  refreshLists();
}

/** The browser hides why a WebSocket closed, so ask the API whether the session still exists. */
async function checkSession() {
  try {
    await api('GET', '/api/me');
  } catch (e) {
    if (!(e instanceof ApiError && e.status === 401) || !state.me) return;
    endSession();
    toast('Your session has ended. Log in again.', 'error');
    navigate('/');
  }
}

function endSession() {
  state.socket?.close();
  Object.assign(state, { me: null, socket: null, game: null, invites: [], online: new Set() });
}

async function refreshLists() {
  [state.players, state.leaderboard] = await Promise.all([api<any>('GET', '/api/players'), api<any>('GET', '/api/leaderboard')]);
  if (route() === 'lobby') render();
}

async function refreshMe() {
  state.me = await api<Me>('GET', '/api/me');
}

function onMessage(msg: ServerMsg) {
  switch (msg.type) {
    case 'online':
      state.online = new Set(msg.user_ids);
      // Someone online we don't know yet has just registered.
      if (msg.user_ids.some((id) => !state.players.some((p) => p.id === id))) refreshLists().then(render);
      if (!state.game || state.game.status === 'waiting') render();
      return;
    case 'invites':
      for (const inv of msg.invites) {
        if (!state.invites.some((old) => old.game_id === inv.game_id)) toast(`${inv.leader} invited you to a game`);
      }
      state.invites = msg.invites;
      if (!state.game || state.game.status !== 'playing') render();
      return;
    case 'game': {
      const prev = state.game;
      state.game = msg.game;
      if (msg.game?.status === 'finished' && prev?.status === 'playing') {
        refreshMe().then(refreshLists);
      }
      if (msg.game && !prev && route() !== 'game') return navigate('/game');
      if (!msg.game && route() === 'game') return navigate('/');
      if (msg.game?.status === 'playing' && prev?.status === 'playing' && board && boardGameId === msg.game.id) {
        updatePlaying(msg.game);
      } else {
        render();
      }
      return;
    }
    case 'claim_result':
      if (msg.result === 'ok') toast('Set!', 'success');
      if (msg.result === 'not_a_set') toast('Not a set: 10 second lockout', 'error');
      if (msg.result === 'stale') toast('Too slow, those cards are gone');
      if (msg.result === 'locked') toast('You are locked out for a moment', 'error');
      board?.clearSelection(msg.result === 'not_a_set');
      return;
    case 'error':
      toast(msg.message, 'error');
      board?.clearSelection();
      return;
    case 'notice':
      toast(msg.message);
      return;
  }
}

function send(msg: Parameters<Socket['send']>[0]) {
  state.socket?.send(msg);
}

function route(): 'login' | 'register' | 'forgot' | 'reset' | 'verify' | 'profile' | 'game' | 'lobby' {
  const p = location.pathname;
  if (p === '/register' || p === '/forgot' || p === '/reset' || p === '/verify') return p.slice(1) as 'register';
  if (!state.me) return 'login';
  if (p === '/profile') return 'profile';
  if (p === '/game' && state.game) return 'game';
  return 'lobby';
}

function render() {
  const r = route();
  if (r !== 'game' || state.game?.status !== 'playing') teardownBoard();
  app.replaceChildren(...(state.me ? [header()] : []), h('main', { class: `page ${r}` }, ...page(r)));
  if (r === 'game' && state.game?.status === 'playing') mountBoard(state.game);
}

function page(r: ReturnType<typeof route>): Node[] {
  switch (r) {
    case 'login':
      return [loginForm()];
    case 'register':
      return [registerForm()];
    case 'forgot':
      return [forgotForm()];
    case 'reset':
      return [resetForm()];
    case 'verify':
      return [verifyPage()];
    case 'profile':
      return profilePage();
    case 'game':
      return gamePage(state.game!);
    case 'lobby':
      return lobbyPage();
  }
}

function header() {
  return h(
    'header',
    {},
    h('a', { class: 'logo', href: '/', onclick: (e: Event) => (e.preventDefault(), navigate(state.game ? '/game' : '/')) }, 'SET'),
    h('span', { id: 'conn', class: state.connected ? 'on' : 'off' }, state.connected ? 'online' : 'connecting…'),
    h('nav', {}, state.game ? link('/game', 'Current game') : null, link('/', 'Lobby'), link('/profile', state.me!.username)),
  );
}

function renderStatus() {
  const el = document.getElementById('conn');
  if (!el) return;
  el.className = state.connected ? 'on' : 'off';
  el.textContent = state.connected ? 'online' : 'connecting…';
}

// ---------- Login and registration ----------

function form(
  title: string,
  fields: HTMLElement[],
  submit: string,
  onSubmit: (data: FormData, form: HTMLFormElement) => Promise<void>,
  footer?: Node,
) {
  const error = h('p', { class: 'form-error' });
  const button = h('button', { type: 'submit' }, submit);
  const el = h(
    'form',
    {
      class: 'card auth',
      onsubmit: async (e: Event) => {
        e.preventDefault();
        error.textContent = '';
        button.disabled = true;
        try {
          await onSubmit(new FormData(el), el);
        } catch (err) {
          error.textContent = err instanceof Error ? err.message : String(err);
        } finally {
          button.disabled = false;
        }
      },
    },
    h('h1', {}, title),
    ...fields,
    error,
    button,
    footer,
  );
  return el;
}

/** A card with a message, shown in place of a form once it is done. */
function notice(title: string, text: string, footer?: Node) {
  return h('section', { class: 'card auth' }, h('h1', {}, title), h('p', {}, text), footer);
}

function field(label: string, name: string, type = 'text', autocomplete = 'off') {
  return h('label', {}, label, h('input', { name, type, required: true, autocomplete }));
}

const tokenParam = () => new URLSearchParams(location.search).get('token') ?? '';

async function loggedIn(me: { id: number; username: string }) {
  state.me = { ...me, email: '', played: 0, wins: 0 };
  await refreshMe();
  startSession();
  navigate('/');
}

function loginForm() {
  return form(
    'Log in',
    [field('Email', 'email', 'email', 'email'), field('Password', 'password', 'password', 'current-password')],
    'Log in',
    async (d) => loggedIn(await api('POST', '/api/login', { email: d.get('email'), password: d.get('password') })),
    h(
      'div',
      {},
      h('p', { class: 'hint' }, 'New here? ', link('/register', 'Create an account')),
      h('p', { class: 'hint' }, link('/forgot', 'Forgot your password?')),
    ),
  );
}

function registerForm() {
  return form(
    'Create your account',
    [
      field('Email', 'email', 'email', 'email'),
      field('Username', 'username', 'text', 'username'),
      field('Password', 'password', 'password', 'new-password'),
    ],
    'Register',
    async (d, el) => {
      await api('POST', '/api/register', { email: d.get('email'), username: d.get('username'), password: d.get('password') });
      el.replaceWith(
        notice(
          'Check your email',
          `We sent a confirmation link to ${d.get('email')}. Follow it within 24 hours to finish creating your account.`,
        ),
      );
    },
    h('p', { class: 'hint' }, 'Already registered? ', link('/', 'Log in')),
  );
}

function forgotForm() {
  return form(
    'Reset your password',
    [field('Email', 'email', 'email', 'email')],
    'Send reset link',
    async (d, el) => {
      await api('POST', '/api/forgot', { email: d.get('email') });
      el.replaceWith(
        notice('Check your email', `If an account uses ${d.get('email')}, we sent it a link to set a new password. The link expires in 1 hour.`),
      );
    },
    h('p', { class: 'hint' }, 'Remembered it? ', link('/', 'Log in')),
  );
}

function resetForm() {
  return form(
    'Choose a new password',
    [field('New password', 'password', 'password', 'new-password')],
    'Set password',
    async (d) => {
      if (state.me) await logout(false);
      await loggedIn(await api('POST', '/api/reset', { token: tokenParam(), password: d.get('password') }));
    },
    h('p', { class: 'hint' }, 'Link expired? ', link('/forgot', 'Get a new one')),
  );
}

/** Confirms the email from the link in the confirmation email, which logs in. */
function verifyPage() {
  const el = notice('Confirming your email…', '');
  (async () => {
    try {
      await loggedIn(await api('POST', '/api/verify', { token: tokenParam() }));
      toast('Email confirmed. Welcome!', 'success');
    } catch (err) {
      el.replaceWith(
        notice(
          'This link does not work',
          `${err instanceof Error ? err.message : err}. If your account is confirmed already, log in; otherwise register again to get a new link.`,
          h('p', { class: 'hint' }, link('/', 'Log in'), ' · ', link('/register', 'Register')),
        ),
      );
    }
  })();
  return el;
}

async function logout(go = true) {
  await api('POST', '/api/logout');
  endSession();
  if (go) navigate('/');
}

// ---------- Lobby ----------

function invitesBox() {
  if (state.invites.length === 0) return null;
  return h(
    'section',
    { class: 'card invites' },
    h('h2', {}, 'Game invitations'),
    ...state.invites.map((inv) =>
      h(
        'div',
        { class: 'row' },
        h('span', {}, h('b', {}, inv.leader), ' invited you to a game'),
        h('button', { onclick: () => send({ type: 'respond_invite', game_id: inv.game_id, accept: true }) }, 'Join'),
        h('button', { class: 'secondary', onclick: () => send({ type: 'respond_invite', game_id: inv.game_id, accept: false }) }, 'Decline'),
      ),
    ),
  );
}

function lobbyPage() {
  const inGame = state.game && state.game.status !== 'finished';
  return [
    invitesBox() ?? h('span'),
    h(
      'section',
      { class: 'card hero' },
      h('h1', {}, `Hi ${state.me!.username}`),
      h('p', {}, `Games played: ${state.me!.played} · Wins: ${state.me!.wins}`),
      inGame
        ? h('button', { onclick: () => navigate('/game') }, 'Back to your game')
        : h('button', { onclick: () => send({ type: 'create_game' }) }, 'New game'),
    ),
    h(
      'div',
      { class: 'columns' },
      h('section', { class: 'card' }, h('h2', {}, 'Players'), playerList()),
      h('section', { class: 'card' }, h('h2', {}, 'Leaderboard'), leaderboardTable()),
    ),
  ];
}

function sortedPlayers() {
  return [...state.players].sort((a, b) => Number(state.online.has(b.id)) - Number(state.online.has(a.id)) || a.username.localeCompare(b.username));
}

function playerList() {
  return h(
    'ul',
    { class: 'players' },
    ...sortedPlayers().map((p) => h('li', {}, h('span', { class: `dot ${state.online.has(p.id) ? 'on' : ''}` }), p.username)),
  );
}

function leaderboardTable() {
  if (state.leaderboard.length === 0) return h('p', { class: 'hint' }, 'No games finished yet.');
  return h(
    'table',
    {},
    h('tr', {}, h('th', {}, '#'), h('th', {}, 'Player'), h('th', {}, 'Wins'), h('th', {}, 'Played'), h('th', {}, 'Win rate')),
    ...state.leaderboard.map((r, i) =>
      h(
        'tr',
        { class: r.username === state.me!.username ? 'me' : '' },
        h('td', {}, i + 1),
        h('td', {}, r.username),
        h('td', {}, r.wins),
        h('td', {}, r.played),
        h('td', {}, `${Math.round((100 * r.wins) / r.played)}%`),
      ),
    ),
  );
}

// ---------- Profile ----------

function profilePage() {
  return [
    h(
      'section',
      { class: 'card' },
      h('h1', {}, state.me!.username),
      h('p', { class: 'hint' }, state.me!.email),
      h('p', {}, `Games played: ${state.me!.played} · Wins: ${state.me!.wins}`),
      h('button', { class: 'secondary', onclick: () => logout() }, 'Log out'),
    ),
  ];
}

// ---------- Game ----------

function playerColor(game: GameView, id: number) {
  return PLAYER_COLORS[Math.max(0, game.players.findIndex((p) => p.id === id)) % PLAYER_COLORS.length];
}

function gamePage(game: GameView): Node[] {
  if (game.status === 'waiting') return waitingRoom(game);
  if (game.status === 'finished') return results(game);
  return [h('div', { id: 'board' }), h('aside', { class: 'side' }, ...sidebar(game))];
}

function waitingRoom(game: GameView) {
  const leader = game.leader === state.me!.id;
  const taken = new Set([...game.players.map((p) => p.id), ...game.invited.map((p) => p.id)]);
  const candidates = sortedPlayers().filter((p) => !taken.has(p.id));
  return [
    h(
      'section',
      { class: 'card' },
      h('h1', {}, leader ? 'Your game' : `${game.players.find((p) => p.id === game.leader)?.username}'s game`),
      h('p', { class: 'hint' }, leader ? 'Invite players, then start when everyone has joined.' : 'Waiting for the leader to start the game.'),
      h('h2', {}, 'Players'),
      h(
        'ul',
        { class: 'players' },
        ...game.players.map((p) =>
          h('li', {}, h('span', { class: `dot ${p.connected ? 'on' : ''}` }), p.username, p.id === game.leader ? h('span', { class: 'tag' }, 'leader') : null),
        ),
        ...game.invited.map((p) => h('li', { class: 'pending' }, h('span', { class: 'dot' }), p.username, h('span', { class: 'tag' }, 'invited'))),
      ),
      h(
        'div',
        { class: 'row' },
        leader ? h('button', { onclick: () => send({ type: 'start_game' }) }, game.players.length === 1 ? 'Start solo game' : 'Start game') : null,
        h('button', { class: 'secondary', onclick: () => send({ type: 'leave_game' }) }, leader ? 'Cancel game' : 'Leave'),
      ),
    ),
    leader
      ? h(
          'section',
          { class: 'card' },
          h('h2', {}, 'Invite players'),
          candidates.length === 0
            ? h('p', { class: 'hint' }, 'Everyone is already invited.')
            : h(
                'ul',
                { class: 'players' },
                ...candidates.map((p) =>
                  h(
                    'li',
                    {},
                    h('span', { class: `dot ${state.online.has(p.id) ? 'on' : ''}` }),
                    p.username,
                    h('button', { class: 'small', onclick: () => send({ type: 'invite', user_id: p.id }) }, 'Invite'),
                  ),
                ),
              ),
        )
      : h('span'),
  ];
}

/** The server sends no update when a lockout ends, so the tag removes itself. */
function lockedTag(ms: number) {
  const tag = h('span', { class: 'tag locked' }, 'locked');
  setTimeout(() => tag.remove(), ms);
  return tag;
}

function scoreList(game: GameView) {
  const players = [...game.players].sort((a, b) => b.score - a.score);
  return h(
    'ul',
    { class: 'scores' },
    ...players.map((p) =>
      h(
        'li',
        { class: [p.id === state.me!.id ? 'me' : '', p.left || !p.connected ? 'away' : ''].join(' ') },
        h('span', { class: 'swatch', style: `background:${hex(playerColor(game, p.id))}` }),
        h('span', { class: 'name' }, p.username, p.left ? ' (left)' : !p.connected ? ' (offline)' : ''),
        p.locked_ms > 0 ? lockedTag(p.locked_ms) : null,
        h('b', {}, p.score),
      ),
    ),
  );
}

function sidebar(game: GameView) {
  return [
    h('h2', {}, 'Scores'),
    scoreList(game),
    h('p', { class: 'deck' }, `Cards left in deck: ${game.deck_remaining}`),
    h('button', { class: 'secondary', onclick: () => confirm('Leave this game? Your score is kept.') && send({ type: 'leave_game' }) }, 'Leave game'),
  ];
}

function mountBoard(game: GameView) {
  const host = document.getElementById('board')!;
  if (board && boardGameId === game.id) {
    host.replaceWith(boardHost!);
  } else {
    teardownBoard();
    boardHost = host;
    boardGameId = game.id;
    const b = new Board(host, (cards) => send({ type: 'claim', cards }));
    board = b;
    b.init().then(() => {
      if (board === b && state.game) updatePlaying(state.game);
    });
    return;
  }
  updatePlaying(game);
}
let boardHost: HTMLElement | null = null;

function teardownBoard() {
  board?.destroy();
  board = null;
  boardGameId = null;
  boardHost = null;
  clearInterval(lockTimer);
}

function updatePlaying(game: GameView) {
  const claim = game.last_claim && { seq: game.last_claim.seq, cards: game.last_claim.cards, color: playerColor(game, game.last_claim.by) };
  board?.setTable(game.table, claim);
  const side = document.querySelector('aside.side');
  side?.replaceChildren(...sidebar(game));
  const me = game.players.find((p) => p.id === state.me!.id);
  const until = performance.now() + (me?.locked_ms ?? 0);
  clearInterval(lockTimer);
  const tick = () => {
    const left = until - performance.now();
    board?.setLocked(left > 0);
    if (left <= 0) clearInterval(lockTimer);
  };
  tick();
  if (me && me.locked_ms > 0) lockTimer = window.setInterval(tick, 100);
}

function results(game: GameView) {
  const names = game.players.filter((p) => game.winners.includes(p.id)).map((p) => p.username);
  const iWon = game.winners.includes(state.me!.id);
  return [
    h(
      'section',
      { class: 'card results' },
      h('h1', {}, iWon ? (game.winners.length > 1 ? 'You tied for the win!' : 'You won!') : `${names.join(' and ')} won`),
      game.players.length === 1 ? h('p', { class: 'hint' }, 'Solo games are practice and are not counted in the statistics.') : null,
      scoreList(game),
      h('button', { onclick: () => send({ type: 'leave_game' }) }, 'Back to lobby'),
    ),
  ];
}

boot();
