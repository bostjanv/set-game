import { Application, Container, Graphics, Sprite, Texture } from 'pixi.js';

/** Base card size in texture pixels; cards are scaled to fit the board. */
const W = 120;
const H = 180;
const GAP = 14;
const PAD = 16;
const CARD_COLORS = [0xd62e4d, 0x1d9a4c, 0x6a3fb5];
const SELECT_COLOR = 0xf5b301;
const ERROR_COLOR = 0xe11d48;
const BORDER_COLOR = 0xd5d8e0;

/** Card numbers encode their attributes in base 3, as on the server. */
export function attrs(card: number) {
  return {
    count: (card % 3) + 1,
    shape: Math.floor(card / 3) % 3, // diamond, oval, squiggle
    color: Math.floor(card / 9) % 3, // red, green, purple
    shading: Math.floor(card / 27) % 3, // solid, striped, open
  };
}

function shapePath(g: Graphics, shape: number, cx: number, cy: number, w: number, h: number) {
  if (shape === 0) {
    g.poly([cx - w / 2, cy, cx, cy - h / 2, cx + w / 2, cy, cx, cy + h / 2]);
  } else if (shape === 1) {
    g.roundRect(cx - w / 2, cy - h / 2, w, h, h / 2);
  } else {
    // A point-symmetric squiggle in unit coordinates: x in [-1, 1], y roughly in [-0.5, 0.5].
    const p = (x: number, y: number): [number, number] => [cx + (x * w) / 2, cy + y * h];
    g.moveTo(...p(-0.92, 0.3));
    g.bezierCurveTo(...p(-1.1, -0.45), ...p(-0.35, -0.72), ...p(0.12, -0.3));
    g.bezierCurveTo(...p(0.42, -0.06), ...p(0.68, -0.6), ...p(0.92, -0.3));
    g.bezierCurveTo(...p(1.1, 0.45), ...p(0.35, 0.72), ...p(-0.12, 0.3));
    g.bezierCurveTo(...p(-0.42, 0.06), ...p(-0.68, 0.6), ...p(-0.92, 0.3));
    g.closePath();
  }
}

function buildCard(card: number): Container {
  const { count, shape, color, shading } = attrs(card);
  const fill = CARD_COLORS[color];
  const c = new Container();
  c.addChild(new Graphics().roundRect(0, 0, W, H, 12).fill(0xffffff));
  const sw = 82, sh = 34, gap = 14;
  const total = count * sh + (count - 1) * gap;
  for (let i = 0; i < count; i++) {
    const cx = W / 2;
    const cy = H / 2 - total / 2 + sh / 2 + i * (sh + gap);
    if (shading === 1) {
      const stripes = new Graphics();
      for (let x = cx - sw / 2; x <= cx + sw / 2; x += 5) stripes.rect(x, cy - sh / 2, 1.6, sh);
      stripes.fill(fill);
      const mask = new Graphics();
      shapePath(mask, shape, cx, cy, sw, sh);
      mask.fill(0xffffff);
      stripes.mask = mask;
      c.addChild(mask, stripes);
    }
    const g = new Graphics();
    shapePath(g, shape, cx, cy, sw, sh);
    if (shading === 0) g.fill(fill);
    g.stroke({ width: 3, color: fill, join: 'round' });
    c.addChild(g);
  }
  return c;
}

interface Slot {
  card: number;
  root: Container;
  frame: Graphics;
  border: Graphics;
}

interface Anim {
  target: Container;
  start: number;
  duration: number;
  update(t: number): void;
  done?(): void;
}

const ease = (t: number) => 1 - (1 - t) ** 3;

export interface ClaimFlash {
  seq: number;
  cards: number[];
  color: number;
}

export class Board {
  private app = new Application();
  private ready = false;
  private textures = new Map<number, Texture>();
  private slots = new Map<number, Slot>();
  private table: number[] = [];
  private selected = new Set<number>();
  private pending = false;
  private locked = false;
  private lastSeq: number | null = null;
  private anims: Anim[] = [];
  private resizer = new ResizeObserver(() => this.resize());

  /** `onSubmit` is called with three cards when the player selects a third card. */
  constructor(private host: HTMLElement, private onSubmit: (cards: number[]) => void) {}

  async init() {
    await this.app.init({
      backgroundAlpha: 0,
      antialias: true,
      resolution: window.devicePixelRatio || 1,
      autoDensity: true,
      width: this.host.clientWidth || 800,
      height: this.host.clientHeight || 600,
    });
    this.host.appendChild(this.app.canvas);
    this.app.ticker.add(() => this.tick());
    this.resizer.observe(this.host);
    this.ready = true;
    this.resize();
  }

  destroy() {
    this.resizer.disconnect();
    if (this.ready) this.app.destroy(true, { children: true });
    for (const t of this.textures.values()) t.destroy(true);
  }

  setTable(table: number[], claim: ClaimFlash | null) {
    const first = this.lastSeq === null;
    const seq = claim?.seq ?? 0;
    const flash = !first && claim && seq !== this.lastSeq ? claim : null;
    this.lastSeq = seq;

    for (const [card, slot] of this.slots) {
      if (table.includes(card)) continue;
      this.slots.delete(card);
      this.selected.delete(card);
      if (flash?.cards.includes(card)) this.flashOut(slot, flash.color);
      else slot.root.destroy({ children: true });
    }
    for (const card of table) {
      if (this.slots.has(card)) continue;
      const slot = this.makeSlot(card);
      this.slots.set(card, slot);
      if (!first) {
        slot.root.alpha = 0;
        this.animate(slot.root, 250, 220, (t) => (slot.root.alpha = t));
      }
    }
    this.table = table;
    this.layout(!first);
    this.redrawFrames();
  }

  /** Clears the selection after the server answered a claim; `wrong` shakes the cards first. */
  clearSelection(wrong = false) {
    const cards = [...this.selected];
    this.pending = false;
    this.selected.clear();
    if (!wrong) return this.redrawFrames();
    for (const card of cards) {
      const slot = this.slots.get(card);
      if (!slot) continue;
      const x = slot.root.x;
      this.drawFrame(slot, ERROR_COLOR, 6);
      this.animate(slot.root, 0, 380, (t) => (slot.root.x = x + Math.sin(t * Math.PI * 6) * 8 * (1 - t)), () => {
        slot.root.x = x;
        this.redrawFrames();
      });
    }
  }

  setLocked(locked: boolean) {
    if (locked === this.locked) return;
    this.locked = locked;
    this.app.stage.alpha = locked ? 0.45 : 1;
  }

  private texture(card: number) {
    let t = this.textures.get(card);
    if (!t) {
      const c = buildCard(card);
      t = this.app.renderer.generateTexture({ target: c, resolution: 3, antialias: true });
      c.destroy({ children: true });
      this.textures.set(card, t);
    }
    return t;
  }

  private makeSlot(card: number): Slot {
    const root = new Container();
    const sprite = new Sprite(this.texture(card));
    sprite.anchor.set(0.5);
    sprite.eventMode = 'static';
    sprite.cursor = 'pointer';
    sprite.on('pointertap', () => this.toggle(card));
    const frame = new Graphics();
    const border = new Graphics();
    border.eventMode = 'none';
    root.addChild(frame, sprite, border);
    this.app.stage.addChild(root);
    return { card, root, frame, border };
  }

  private toggle(card: number) {
    if (this.pending || this.locked) return;
    if (this.selected.has(card)) this.selected.delete(card);
    else this.selected.add(card);
    this.redrawFrames();
    if (this.selected.size === 3) {
      this.pending = true;
      this.onSubmit([...this.selected]);
    }
  }

  private drawFrame(slot: Slot, color: number, width: number) {
    slot.frame.clear().roundRect(-W / 2 - 5, -H / 2 - 5, W + 10, H + 10, 16).stroke({ width, color });
  }

  private redrawFrames() {
    for (const slot of this.slots.values()) {
      slot.frame.clear();
      const color = this.selected.has(slot.card) ? SELECT_COLOR : BORDER_COLOR;
      slot.border.clear().roundRect(-W / 2, -H / 2, W, H, 12).stroke({ width: 2, color });
    }
  }

  private flashOut(slot: Slot, color: number) {
    this.drawFrame(slot, color, 8);
    slot.root.eventMode = 'none';
    const s = slot.root.scale.x;
    this.animate(slot.root, 150, 400, (t) => {
      slot.root.alpha = 1 - t;
      slot.root.scale.set(s * (1 + 0.12 * t));
    }, () => slot.root.destroy({ children: true }));
  }

  private resize() {
    if (!this.ready) return;
    this.app.renderer.resize(this.host.clientWidth, this.host.clientHeight);
    this.layout(false);
  }

  /** Picks the column count that makes cards largest, then centers the grid. */
  private layout(animate: boolean) {
    if (!this.ready) return;
    const n = this.table.length;
    if (n === 0) return;
    const bw = this.app.renderer.width / this.app.renderer.resolution;
    const bh = this.app.renderer.height / this.app.renderer.resolution;
    let best = { cols: 1, rows: n, w: 0 };
    for (let cols = 1; cols <= n; cols++) {
      const rows = Math.ceil(n / cols);
      const w = Math.min((bw - 2 * PAD - GAP * (cols - 1)) / cols, ((bh - 2 * PAD - GAP * (rows - 1)) / rows) * (W / H));
      if (w > best.w) best = { cols, rows, w };
    }
    const cw = Math.max(best.w, 10);
    const ch = (cw * H) / W;
    const gridW = best.cols * cw + (best.cols - 1) * GAP;
    const gridH = best.rows * ch + (best.rows - 1) * GAP;
    const x0 = (bw - gridW) / 2 + cw / 2;
    const y0 = (bh - gridH) / 2 + ch / 2;
    this.table.forEach((card, i) => {
      const slot = this.slots.get(card)!;
      const x = x0 + (i % best.cols) * (cw + GAP);
      const y = y0 + Math.floor(i / best.cols) * (ch + GAP);
      const scale = cw / W;
      const moved = slot.root.x !== 0 && (slot.root.x !== x || slot.root.y !== y);
      if (animate && moved) {
        const [fx, fy, fs] = [slot.root.x, slot.root.y, slot.root.scale.x];
        this.animate(slot.root, 0, 260, (t) => {
          slot.root.position.set(fx + (x - fx) * t, fy + (y - fy) * t);
          slot.root.scale.set(fs + (scale - fs) * t);
        });
      } else {
        slot.root.position.set(x, y);
        slot.root.scale.set(scale);
      }
    });
  }

  private animate(target: Container, delay: number, duration: number, update: (t: number) => void, done?: () => void) {
    update(0);
    this.anims.push({ target, start: performance.now() + delay, duration, update, done });
  }

  private tick() {
    const now = performance.now();
    this.anims = this.anims.filter((a) => {
      if (a.target.destroyed) return false;
      if (now < a.start) return true;
      const t = Math.min(1, (now - a.start) / a.duration);
      a.update(ease(t));
      if (t < 1) return true;
      a.done?.();
      return false;
    });
  }
}
