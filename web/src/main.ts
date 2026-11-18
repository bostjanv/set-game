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
  try {
    state.me = await api<Me>('GET', '/api/me');
  } catch (e) {
    if (!(e instanceof ApiError && e.status === 401)) throw e;
  }
  if (state.me) startSession();
  render();
}

function startSession() {
  state.socket = new Socket(onMessage, (connected) => {
    state.connected = connected;
    renderStatus();
  });
  refreshLists();
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
      if (msg.result === 'not_a_set') toast('Not a set: 3 second lockout', 'error');
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

function route(): 'login' | 'register' | 'profile' | 'game' | 'lobby' {
  const p = location.pathname;
  if (p === '/register') return 'register';
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

function form(title: string, fields: HTMLElement[], submit: string, onSubmit: (data: FormData) => Promise<void>, footer?: Node) {
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
          await onSubmit(new FormData(el));
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

function field(label: string, name: string, type = 'text', value = '') {
  return h('label', {}, label, h('input', { name, type, value, required: true, autocomplete: type === 'password' ? 'current-password' : 'off' }));
}

async function loggedIn(me: { id: number; username: string }) {
  state.me = { ...me, played: 0, wins: 0 };
  await refreshMe();
  startSession();
  navigate('/');
}

function loginForm() {
  return form(
    'Log in',
    [field('Username', 'username'), field('Password', 'password', 'password')],
    'Log in',
    async (d) => loggedIn(await api('POST', '/api/login', { username: d.get('username'), password: d.get('password') })),
    h('p', { class: 'hint' }, 'New here? Ask a player for an invite link.'),
  );
}

function registerForm() {
  const code = new URLSearchParams(location.search).get('code') ?? '';
  return form(
    'Create your account',
    [field('Invite code', 'code', 'text', code), field('Username', 'username'), field('Password', 'password', 'password')],
    'Register',
    async (d) => {
      if (state.me) await logout(false);
      await loggedIn(
        await api('POST', '/api/register', { code: d.get('code'), username: d.get('username'), password: d.get('password') }),
      );
    },
    h('p', { class: 'hint' }, 'Already registered? ', link('/', 'Log in')),
  );
}

async function logout(go = true) {
  await api('POST', '/api/logout');
  state.socket?.close();
  Object.assign(state, { me: null, socket: null, game: null, invites: [], online: new Set() });
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
  const list = h('ul', { class: 'codes' }, h('li', { class: 'hint' }, 'Loading…'));
  const load = async () => {
    const codes = await api<{ code: string; used_by: string | null }[]>('GET', '/api/invites');
    list.replaceChildren(
      ...(codes.length ? [] : [h('li', { class: 'hint' }, 'No invite codes yet.')]),
      ...codes.map((c) => {
        const url = `${location.origin}/register?code=${c.code}`;
        return h(
          'li',
          {},
          h('code', {}, c.code),
          c.used_by
            ? h('span', { class: 'used' }, `used by ${c.used_by}`)
            : h('button', { class: 'secondary', onclick: () => navigator.clipboard.writeText(url).then(() => toast('Link copied', 'success')) }, 'Copy link'),
        );
      }),
    );
  };
  load();
  return [
    h(
      'section',
      { class: 'card' },
      h('h1', {}, state.me!.username),
      h('p', {}, `Games played: ${state.me!.played} · Wins: ${state.me!.wins}`),
      h('button', { class: 'secondary', onclick: () => logout() }, 'Log out'),
    ),
    h(
      'section',
      { class: 'card' },
      h('h2', {}, 'Invite a new player'),
      h('p', { class: 'hint' }, 'Each code registers one account. Share the link with the person you invite.'),
      h('button', { onclick: async () => (await api('POST', '/api/invites'), load()) }, 'Create invite code'),
      list,
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
            ? h('p', { class: 'hint' }, 'Everyone is already invited. Invite new people from your profile.')
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
        p.locked_ms > 0 ? h('span', { class: 'tag locked' }, 'locked') : null,
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
    h('p', { id: 'lock', class: 'lock' }),
    h('p', { class: 'hint' }, 'Click three cards that form a set.'),
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
    const el = document.getElementById('lock');
    if (el) el.textContent = left > 0 ? `Locked out: ${(left / 1000).toFixed(1)} s` : '';
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
