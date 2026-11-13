"use strict";

/* Storage UI. Talks to the Rust side through three commands:
   scan(onProgress) → Report, clean(keys, onProgress) → summary, disk() → {total, free}.
   The UI only ever sends entry keys; the backend decides what they point at. */

const TAURI = window.__TAURI__;
const invoke = (cmd, args) => TAURI.core.invoke(cmd, args);
const $ = (s, el = document) => el.querySelector(s);
const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

/* ---------- categories ---------- */

// 7×7 pixel glyphs, drawn in the category colour to match the dot matrix.
const GLYPHS = {
  worktrees: [".#...#.", ".#...#.", ".#...#.", ".#..#..", ".#.#...", ".##....", ".#....."],
  agents: ["...#...", "...#...", "..###..", "#######", "..###..", "...#...", "...#..."],
  code: [".......", "#......", ".#.....", "..#....", ".#.....", "#..###.", "......."],
  deps: ["#######", "#..#..#", "#######", "#.....#", "#.....#", "#.....#", "#######"],
  build: [".####..", ".#####.", ".####..", "...#...", "...#...", "...#...", "...#..."],
  caches: ["...##..", "..##...", ".##....", "######.", "...##..", "..##...", ".##...."],
  toolchains: [".#...#.", ".##.##.", "..###..", "...#...", "...#...", "...#...", "...#..."],
  models: [".#.#.#.", ".#####.", "##...##", ".#.#.#.", "##...##", ".#####.", ".#.#.#."],
  containers: ["..###..", "..#.#..", "..###..", "###.###", "#.#.#.#", "###.###", "......."],
  trash: ["..###..", "#######", ".#...#.", ".#.#.#.", ".#.#.#.", ".#.#.#.", ".#####."],
  other: [".......", "###....", "#######", "#.....#", "#.....#", "#######", "......."],
  system: ["...#...", ".#####.", ".##.##.", "###.###", ".##.##.", ".#####.", "...#..."],
  all: ["...#...", "...#...", ".#.#.#.", "..###..", "#######", "..###..", ".#.#.#."],
};

const CATS = [
  { id: "worktrees", name: "Agent worktrees", color: "#ff4d9d", desc: "Checkouts coding agents make for each task, each with its own dependencies and build output." },
  { id: "agents", name: "Agent data", color: "#a472ff", desc: "Agent apps, plus the transcripts, logs, temp files and CLI versions Claude Code, Codex, T3 Code and others keep." },
  { id: "code", name: "Projects", color: "#4d82ff", desc: "Your source code, not counting dependencies and build output." },
  { id: "deps", name: "Dependencies", color: "#22d0f0", desc: "node_modules and Python virtual environments. Reinstalled from your lockfiles." },
  { id: "build", name: "Build output", color: "#3ddb7a", desc: "target, .next, DerivedData and other build folders. Recreated on the next build." },
  { id: "caches", name: "Caches & temp", color: "#ff8a2a", desc: "Package downloads, app caches and temporary files. Fetched or rebuilt when they're needed." },
  { id: "toolchains", name: "Tools & SDKs", color: "#f5c623", desc: "Xcode, editors, simulator runtimes, Homebrew, language versions and Android images." },
  { id: "models", name: "AI models", color: "#a6e33b", desc: "Local model weights from Ollama, LM Studio and Hugging Face." },
  { id: "containers", name: "Containers & VMs", color: "#2ec4b6", desc: "Docker, OrbStack and virtual machine disk images." },
  { id: "trash", name: "Trash", color: "#f2484b", desc: "Files already deleted. Empty the Trash to get the space back." },
  { id: "other", name: "Everything else", color: "#b4b4bc", desc: "Documents, media and app data in your home folder." },
  { id: "system", name: "System & apps", color: "#6e6e78", desc: "macOS, your other apps, and anything Storage can't read." },
];
const CAT = Object.fromEntries(CATS.map((c) => [c.id, c]));

function glyph(id, size = 15) {
  const c = CAT[id] || { color: "#f3f3f5" };
  const rects = GLYPHS[id].flatMap((row, y) => [...row].map((ch, x) => (ch === "#" ? `<rect x="${x}" y="${y}" width="1" height="1"/>` : ""))).join("");
  return `<svg class="pix" width="${size}" height="${size}" viewBox="0 0 7 7" fill="${c.color}" aria-hidden="true">${rects}</svg>`;
}

/* ---------- formatting ---------- */

function fmt(n) {
  if (n >= 1e12) return (n / 1e12).toFixed(2) + " TB";
  if (n >= 1e9) return (n / 1e9).toFixed(1) + " GB";
  if (n >= 1e6) return Math.round(n / 1e6) + " MB";
  if (n >= 1e3) return Math.round(n / 1e3) + " KB";
  return Math.round(n) + " B";
}
const gb = (n) => (n / 1e9).toFixed(1) + " GB";
const diskSize = (n) => (n >= 1e12 ? +(n / 1e12).toFixed(1) + " TB" : Math.round(n / 1e9) + " GB");
const plural = (n, one, many = one + "s") => `${n.toLocaleString()} ${n === 1 ? one : many}`;
function ago(seconds) {
  const m = seconds / 60, h = m / 60, d = h / 24;
  if (m < 2) return "just now";
  if (h < 1) return Math.round(m) + " min ago";
  if (d < 1) return Math.round(h) + " h ago";
  if (d < 45) return plural(Math.round(d), "day") + " ago";
  if (d < 365) return plural(Math.round(d / 30), "month") + " ago";
  return (Math.round(d / 36.5) / 10) + " years ago";
}
const idleText = (days) => (days == null ? "" : days < 3 ? "used recently" : "idle " + ago(days * 86400).replace(" ago", ""));
const monthYear = (t) => new Date(t * 1000).toLocaleDateString(undefined, { month: "short", year: "numeric" });

const CHEV = '<svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const CHECK = '<svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><path d="m3 8.5 3.2 3L13 4.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const FINDER = '<svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><path d="M9.5 2.5h4v4M13.5 2.5 8 8M12 9.5v3a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h3" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const PIN = '<svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><path d="M5.5 2.5h5l-.6 4.2 2.1 2.3v1H4v-1l2.1-2.3zM8 10v3.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const STOP = '<svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><rect x="4" y="4" width="8" height="8" rx="1.6" fill="currentColor"/></svg>';
const RESCAN = '<svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><path d="M13.2 6.2A5.5 5.5 0 0 0 3 4.6M2.8 9.8A5.5 5.5 0 0 0 13 11.4M3 1.8v2.9h2.9M13 14.2v-2.9h-2.9" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const SPINNER = `<span class="spinner">${Array.from({ length: 8 }, (_, i) => `<i style="transform:rotate(${i * 45}deg);animation-delay:${(i - 8) * 0.1125}s"></i>`).join("")}</span>`;

/* ---------- state ---------- */

const ACTIVE_SECONDS = 2 * 3600; // a worktree touched this recently is probably still in use

const S = {
  phase: "idle", // idle | scanning | ready | cleaning | error
  error: "",
  disk: { total: 0, free: 0 },
  cats: {},
  scanned: 0,
  files: 0,
  scanPath: "",
  usedAtScan: 0,
  system: 0,
  freed: 0,
  report: null,
  recs: new Map(),
  entries: new Map(),
  sel: new Set(),
  open: new Set(),
  tab: "clean",
  hover: null,
  current: null,
  sugIdx: 0,
  progress: 0, // 0..1 while scanning, when an earlier scan says how much there is to do
  live: false, // the bar fills from the scan itself, because there's no earlier result to show
  last: null, // summary of the last finished scan, kept across launches
  result: null, // outcome of the last cleanup
  showIssues: false,
  done: 0,
  todo: 0,
  filter: "",
  wtFilter: "all",
  wtAll: false,
  keep: new Set(), // entry keys the user asked never to have suggested, kept across launches
  more: new Map(), // how many rows a long list shows
  intro: false, // first launch without Full Disk Access: explain before scanning
};

const REDUCED = window.matchMedia("(prefers-reduced-motion: reduce)");

/* The last scan's totals, so the next launch has something to show (and a progress figure) straight away.
   Storage can be unavailable; nothing depends on it. */
const LAST_KEY = "storage.lastScan";
function loadLast() {
  try {
    const v = JSON.parse(localStorage.getItem(LAST_KEY));
    return v && v.cats && v.files ? v : null;
  } catch (_) {
    return null;
  }
}
function saveLast(r) {
  // The entries are for display at the next launch only; nothing can be cleared from them.
  S.last = { t: r.generated, scanned: r.scanned, files: r.files, seconds: r.seconds, cats: r.cats, entries: r.entries };
  storeLast();
}
function storeLast() {
  try {
    localStorage.setItem(LAST_KEY, JSON.stringify(S.last));
  } catch (_) { /* fine */ }
}
/** After a cleanup: what's remembered for the next launch is what is still there, not what was scanned. */
function rememberNow() {
  if (!S.last) return;
  S.last.entries = [...S.entries.values()];
  S.last.cats = { ...S.cats };
  storeLast();
}

const now = () => Date.now() / 1000;
const recOf = (e) => S.recs.get(e.rec);
const methodOf = (e) => (e.rec === "trash" ? "empty" : e.cmd ? "command" : recOf(e).tier === "safe" ? "delete" : "trash");
const wtOf = (e) => (e.parent ? S.entries.get(e.parent) : e.rec === "worktrees" ? e : null);
/** A worktree (or something in one) that a process is sitting in, or that git touched in the last two hours. */
function inUse(e) {
  const w = wtOf(e);
  return !!(w && w.worktree && (w.worktree.busy || now() - w.worktree.last_active < ACTIVE_SECONDS));
}
/** Finished: git was asked and answered, nothing is uncommitted, unreachable or ignored-but-irreplaceable
    (`.env`, local databases), and the work is merged or its upstream branch is gone. Anything git couldn't
    vouch for (unknown, orphaned, locked) is not. */
function wtRemovable(e) {
  const w = e.worktree;
  return !!w && !inUse(e) && !S.keep.has(e.key) && !w.unknown && !w.orphaned && !w.locked
    && w.dirty === 0 && w.unique === 0 && !w.ignored && (w.merged || w.gone);
}
/** Whether an entry may be swept up by a suggestion or a group checkbox, rather than picked by hand. */
function autoOk(e) {
  return !inUse(e) && !e.hold && !S.keep.has(e.key) && !(e.idle_days != null && e.idle_days < 3);
}
/** Why it isn't, in a few words. */
function heldWhy(e) {
  if (S.keep.has(e.key)) return "kept";
  if (e.hold) return e.hold;
  if (inUse(e)) return e.parent ? "in a worktree that's in use" : "in use";
  if (e.idle_days != null && e.idle_days < 3) return "used recently";
  return "";
}
/** A worktree with data Storage couldn't look through (unreadable, or too much of it). It can't
    show that nothing changed there, so cleanup always declines it; the UI doesn't offer it. */
const uncheckable = (e) => !!(e && e.worktree && e.worktree.partial);
/** Whether macOS let the scan read the Trash. Without Full Disk Access it can't be emptied from here either. */
const trashLocked = () => !!S.report && S.report.trash_readable === false;
const usedNow = () => Math.max(0, S.usedAtScan - S.freed);
const isBusy = () => S.phase === "scanning" || S.phase === "cleaning";
const bytesOf = (id) => (S.cats[id] || 0) + (id === "system" ? S.system : 0);
/** `~/path` for home-relative paths; paths outside home are absolute already. */
const disp = (p) => (p.startsWith("/") ? p : "~/" + p);

/** Selected keys minus anything inside a selected worktree (it goes with the worktree). */
function effective(keys) {
  const out = [];
  for (const k of keys) {
    const e = S.entries.get(k);
    if (e && !(e.parent && keys.has(e.parent))) out.push(e);
  }
  return out;
}
const sum = (es) => es.reduce((a, e) => a + e.size, 0);

/* ---------- dot-matrix bar ---------- */

const PITCH = 7, DOT = 4.4, PAD = 7;
const SHIMMER_MS = 8000; // how long the idle shimmer runs after the last change or hover

class DotBar {
  constructor(card, slot, canvas) {
    this.card = card;
    this.slot = slot;
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.w = Object.fromEntries(CATS.map((c) => [c.id, 0]));
    this.parts = [];
    this.shownUsed = 0;
    this.sweep = 0;
    this.last = performance.now();
    this.onscreen = true;
    this.raf = 0;
    this.timer = 0;
    this.lively = 0; // the idle shimmer runs until this time, then the bar holds still
    // Resizing clears the canvas, so redraw before the browser paints rather than a frame later.
    new ResizeObserver(() => { this.resize(); this.now(); }).observe(card);
    // Nothing is drawn while the window is hidden or the bar is scrolled out of view.
    new IntersectionObserver((es) => { this.onscreen = es[es.length - 1].isIntersecting; this.wake(); }).observe(card);
    document.addEventListener("visibilitychange", () => this.wake());
    window.addEventListener("focus", () => this.wake());
    REDUCED.addEventListener("change", () => this.wake());
    this.resize();
    this.now();
  }

  /** Draws on the next frame. Call after anything the bar shows has changed. */
  wake() {
    clearTimeout(this.timer);
    this.timer = 0;
    this.lively = performance.now() + SHIMMER_MS;
    if (!this.raf) this.raf = requestAnimationFrame((t) => this.frame(t));
  }

  /** Draws immediately. */
  now() {
    clearTimeout(this.timer);
    cancelAnimationFrame(this.raf);
    this.timer = 0;
    this.frame(performance.now());
  }

  resize() {
    const dpr = window.devicePixelRatio || 1;
    this.dpr = dpr;
    this.W = this.card.clientWidth;
    this.H = this.card.clientHeight;
    this.canvas.width = Math.round(this.W * dpr);
    this.canvas.height = Math.round(this.H * dpr);
    const b = { x: this.slot.offsetLeft, y: this.slot.offsetTop, w: this.slot.offsetWidth, h: this.slot.offsetHeight };
    this.bar = b;
    this.cols = Math.max(1, Math.floor((b.w - 2 * PAD + (PITCH - DOT)) / PITCH));
    this.rows = Math.max(1, Math.floor((b.h - 2 * PAD + (PITCH - DOT)) / PITCH));
    this.ox = b.x + (b.w - (this.cols * PITCH - (PITCH - DOT))) / 2;
    this.oy = b.y + (b.h - (this.rows * PITCH - (PITCH - DOT))) / 2;
    const n = this.cols * this.rows;
    if (!this.seed || this.seed.length !== n) {
      this.seed = Float32Array.from({ length: n }, Math.random);
      this.phase = Float32Array.from({ length: n }, Math.random);
    }
  }

  targets() {
    const total = S.disk.total || 1;
    return CATS.map((c) => [c.id, bytesOf(c.id) / total]);
  }

  /** Segment spans in px, from the animated widths. */
  spans() {
    const { x, w } = this.bar;
    let acc = 0;
    return CATS.map((c) => {
      const x0 = x + acc * w;
      acc += this.w[c.id];
      return { id: c.id, x0, x1: x + acc * w };
    });
  }

  /** Particles rising from the part of a segment that's being cleared. */
  dissolve(id, bytes) {
    const span = this.spans().find((s) => s.id === id);
    if (!span || span.x1 - span.x0 < 0.5) return;
    const width = Math.min(span.x1 - span.x0, Math.max(PITCH * 1.5, (bytes / (S.disk.total || 1)) * this.bar.w));
    const n = Math.min(260, Math.round((width / PITCH) * this.rows * 0.7) + 14);
    this.spawn(id, span.x1 - width, span.x1, n);
  }

  spawn(id, x0, x1, n) {
    if (REDUCED.matches) return;
    const color = CAT[id].color;
    for (let i = 0; i < n && this.parts.length < 1400; i++) {
      const col = Math.floor((x0 + Math.random() * (x1 - x0) - this.ox) / PITCH);
      const row = Math.floor(Math.random() * this.rows);
      const max = 700 + Math.random() * 1300;
      this.parts.push({
        x: this.ox + col * PITCH, y: this.oy + row * PITCH,
        vx: (Math.random() - 0.5) * 0.5, vy: -(0.15 + Math.random() * 0.9),
        life: max, max, hot: Math.random() < 0.18, color, size: 1.6 + Math.random() * 2,
        delay: Math.random() * 380,
      });
    }
  }

  frame(t) {
    this.raf = 0;
    if (document.hidden || !this.onscreen) return; // `wake` starts it again
    const still = REDUCED.matches;
    const dt = Math.max(0, Math.min(64, t - this.last));
    this.last = t;
    const k = still ? 1 : 1 - Math.exp(-dt / 240);
    let moving = false;
    for (const [id, target] of this.targets()) {
      const d = target - this.w[id];
      if (Math.abs(d) < 1e-5) this.w[id] = target;
      else {
        this.w[id] += d * k;
        moving = true;
      }
    }
    const ds = S.progress - this.sweep;
    this.sweep = ds < 0 || still ? S.progress : this.sweep + ds * (1 - Math.exp(-dt / 180));

    // header number
    const used = usedNow();
    this.shownUsed = still ? used : this.shownUsed + (used - this.shownUsed) * (1 - Math.exp(-dt / 260));
    if (Math.abs(used - this.shownUsed) < 5e6) this.shownUsed = used;
    else moving = true;
    const text = gb(this.shownUsed);
    if (text !== this.usedText) $("#usedNum").textContent = this.usedText = text;

    this.draw(t, dt, still);
    // Full rate while something is changing. After that the shimmer carries on at a low rate for
    // a few seconds and then the bar holds still, drawing nothing until the next `wake` (a hover,
    // a change, the window coming back). With reduced motion it holds still straight away.
    const active = moving || this.parts.length > 0 || isBusy();
    if (active) {
      this.lively = t + SHIMMER_MS;
      this.raf = requestAnimationFrame((tt) => this.frame(tt));
    } else if (!still && t < this.lively && document.hasFocus()) {
      this.timer = setTimeout(() => { this.timer = 0; if (!this.raf) this.raf = requestAnimationFrame((tt) => this.frame(tt)); }, 66);
    }
  }

  draw(t, dt, still) {
    const { ctx, dpr, bar } = this;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, this.W, this.H);
    const R = 9;

    // container + free space
    ctx.beginPath();
    ctx.roundRect(bar.x + 0.5, bar.y + 0.5, bar.w - 1, bar.h - 1, R);
    ctx.fillStyle = "rgba(255,255,255,0.012)";
    ctx.fill();
    ctx.strokeStyle = "rgba(255,255,255,0.06)";
    ctx.lineWidth = 1;
    ctx.stroke();

    const spans = this.spans().filter((s) => s.x1 - s.x0 >= 0.35);
    const usedEnd = spans.length ? spans[spans.length - 1].x1 : bar.x;
    const hover = S.hover;
    const scanning = S.phase === "scanning";
    // Filling up from nothing (first scan), or sweeping across what the last scan found.
    const filling = scanning && S.live;
    const sweepX = scanning && !S.live ? bar.x + this.sweep * (usedEnd - bar.x) : null;

    // dots
    let si = 0;
    for (let c = 0; c < this.cols; c++) {
      const dx = this.ox + c * PITCH;
      const mid = dx + DOT / 2;
      while (si < spans.length && mid >= spans[si].x1) si++;
      const span = si < spans.length && mid >= spans[si].x0 ? spans[si] : null;
      const edge = !still && span && (filling ? mid > usedEnd - PITCH * 2.2 : sweepX != null && Math.abs(mid - sweepX) < PITCH * 1.3);
      ctx.fillStyle = edge ? "#e8f1ff" : span ? CAT[span.id].color : "#ffffff";
      const stale = sweepX != null && mid > sweepX ? 0.4 : 1; // not rescanned yet
      const dim = (hover && span && span.id !== hover ? 0.32 : 1) * stale;
      for (let r = 0; r < this.rows; r++) {
        const i = c * this.rows + r;
        let a;
        if (!span) a = 0.045;
        else if (edge) a = 0.55 + 0.45 * Math.random();
        else {
          const depth = (r + 1) / this.rows;
          const lit = this.seed[i] < 0.07 + 0.6 * depth ** 2.4;
          const ph = this.phase[i];
          if (still) a = lit ? 0.85 : 0.15;
          else if (lit) a = 0.55 + 0.45 * Math.sin(t * 0.0013 * (0.4 + ph) + ph * 40) ** 2;
          else a = (t * 0.00045 + ph * 13) % 9 < 0.05 ? 0.95 : 0.15;
          if (span.id === hover) a = Math.min(1, a * 1.25 + 0.12);
          a *= dim;
        }
        ctx.globalAlpha = a;
        ctx.fillRect(dx, this.oy + r * PITCH, DOT, DOT);
      }
    }
    ctx.globalAlpha = 1;

    // segment tint, outline and glow
    ctx.save();
    ctx.beginPath();
    ctx.roundRect(bar.x, bar.y, bar.w, bar.h, R);
    ctx.clip();
    spans.forEach((s, i) => {
      const c = CAT[s.id].color;
      const on = hover === s.id;
      const faded = hover && !on;
      ctx.globalAlpha = faded ? 0.35 : 1;
      ctx.fillStyle = c + (on ? "2a" : "12");
      ctx.fillRect(s.x0, bar.y, s.x1 - s.x0, bar.h);
      ctx.shadowColor = c;
      ctx.shadowBlur = on ? 18 : 9;
      ctx.strokeStyle = on ? "#ffffff" : c + "e6";
      ctx.lineWidth = on ? 1.4 : 1.1;
      const first = i === 0, lastSpan = i === spans.length - 1;
      ctx.beginPath();
      ctx.roundRect(s.x0 + 0.6, bar.y + 0.6, s.x1 - s.x0 - 1.2, bar.h - 1.2, [first ? R : 2, lastSpan ? R - 2 : 2, lastSpan ? R - 2 : 2, first ? R : 2]);
      ctx.stroke();
    });
    ctx.restore();

    if (scanning && spans.length && !still) {
      const at = Math.min(filling ? usedEnd : sweepX, bar.x + bar.w - 4);
      const g = ctx.createLinearGradient(at - 26, 0, at + 4, 0);
      g.addColorStop(0, "rgba(90,150,255,0)");
      g.addColorStop(1, "rgba(120,170,255,0.35)");
      ctx.fillStyle = g;
      ctx.fillRect(Math.max(bar.x, at - 26), bar.y, 30, bar.h);
    }

    // particles
    const alive = [];
    for (const p of this.parts) {
      if (p.delay > 0) {
        p.delay -= dt;
        alive.push(p);
        continue;
      }
      p.life -= dt;
      if (p.life <= 0) continue;
      const f = dt / 16;
      p.vy -= 0.014 * f;
      p.vx += (Math.random() - 0.5) * 0.06 * f;
      p.x += p.vx * f;
      p.y += p.vy * f;
      if (p.y < bar.y - 14) continue; // gone before it reaches the text above the bar
      const a = Math.pow(p.life / p.max, 0.9) * Math.min(1, (p.y - (bar.y - 14)) / 14);
      ctx.globalAlpha = a;
      ctx.fillStyle = p.hot && p.life / p.max > 0.6 ? "#fff3e0" : p.color;
      ctx.fillRect(p.x, p.y, p.size, p.size);
      alive.push(p);
    }
    ctx.globalAlpha = 1;
    this.parts = alive;
  }

  segmentAt(clientX, clientY) {
    const r = this.card.getBoundingClientRect();
    const x = clientX - r.left, y = clientY - r.top;
    const b = this.bar;
    if (x < b.x || x > b.x + b.w || y < b.y || y > b.y + b.h) return null;
    const s = this.spans().find((s) => x >= s.x0 && x < s.x1 && s.x1 - s.x0 >= 0.35);
    return s ? s.id : null;
  }
}

/* ---------- hero: status, legend, suggestions ---------- */

function renderStatus() {
  const el = $("#status");
  let html;
  if (S.hover) {
    const c = CAT[S.hover];
    html = `${glyph(c.id)}<b>${esc(c.name)}</b><span class="muted num">${fmt(bytesOf(c.id))}</span><span class="desc">${esc(c.desc)}</span>`;
  } else if (S.phase === "scanning") {
    const pct = S.progress > 0 ? `<span class="num">${Math.round(S.progress * 100)}%</span>` : "";
    const files = S.files ? `<span class="dim num">${S.files.toLocaleString()} files</span>` : "";
    html = `<span>Scanning</span>${pct}${files}<span class="path">${esc(S.scanPath && !S.scanPath.startsWith("/") && S.scanPath.includes("/") ? "~/" + S.scanPath : S.scanPath)}</span>`;
  } else if (S.phase === "cleaning" && S.current) {
    html = `<span>${esc(S.current.verb)}</span><span class="path">${esc(S.current.path)}</span>`;
  } else if (S.phase === "error") {
    html = `<span class="err">${esc(S.error)}</span>`;
  } else if (S.report) {
    const un = S.report.unreadable.length;
    const total = Math.max(un, S.report.unreadable_total || 0);
    const hidden = un && S.hideAccess ? `<span class="dim">·</span><button type="button" class="linkbtn" data-act="showaccess">${total.toLocaleString()} unreadable ${total === 1 ? "folder" : "folders"}</button>` : "";
    html = `<span class="muted num">${fmt(Math.max(0, S.disk.total - usedNow()))} available</span><span class="dim num">· ${plural(S.report.files, "file")} scanned in ${Math.max(1, Math.round(S.report.seconds))} s</span>${hidden}`;
  } else {
    html = S.disk.total ? `<span class="muted num">${fmt(S.disk.free)} available</span>` : "";
  }
  if (el.innerHTML !== html) el.innerHTML = html;
}

function renderHeader() {
  $("#usedOf").textContent = S.disk.total ? `of ${diskSize(S.disk.total)} used` : "";
  const scanning = S.phase === "scanning";
  const btn = $("#rescan");
  btn.disabled = S.phase === "cleaning";
  const label = scanning ? "Stop" : S.report ? "Rescan" : "Scan";
  if (btn.dataset.label !== label) {
    btn.dataset.label = label;
    btn.innerHTML = (scanning ? STOP : RESCAN) + label;
    btn.title = scanning ? "Stop scanning (Esc)" : "Scan again (⌘R)";
  }
}

function renderLegend() {
  const el = $("#legend");
  if (!el.children.length) {
    el.innerHTML = CATS.map((c) => `<button type="button" class="lg" data-cat="${c.id}">${glyph(c.id)}<span class="n">${esc(c.name)}</span><span class="s num"></span></button>`).join("");
  }
  for (const b of el.children) {
    const n = bytesOf(b.dataset.cat);
    b.querySelector(".s").textContent = n < 5e7 ? "–" : gb(n);
    b.classList.toggle("zero", n < 5e7);
    b.classList.toggle("on", S.hover === b.dataset.cat);
  }
  $("#hero").classList.toggle("hovering", !!S.hover);
}

const SUGGEST = {
  caches: (b) => [`Caches are using <b>${fmt(b)}</b>. Tools rebuild them when needed.`, "Clear caches", "caches"],
  build: (b, n) => [`Build output in ${plural(n, "folder")} is using <b>${fmt(b)}</b>. Rebuilt on the next build.`, "Clear build output", "build output"],
  deps: (b, n) => [`node_modules in ${plural(n, "project")} idle for a month use <b>${fmt(b)}</b>.`, "Clear node_modules", "idle node_modules"],
  agents: (b) => [`Agent logs, scratch files and old CLI versions use <b>${fmt(b)}</b>.`, "Clear agent leftovers", "agent leftovers"],
  toolchains: (b) => [`Old tool versions and device support files use <b>${fmt(b)}</b>.`, "Clear them", "old tool versions"],
  worktrees: (b, n) => [`${plural(n, "finished worktree")} with nothing uncommitted ${n === 1 ? "uses" : "use"} <b>${fmt(b)}</b>.`, "Move to Trash"],
  trash: (b) => [`The Trash is holding <b>${fmt(b)}</b>.`, "Empty Trash"],
};

/** Whether a safe entry is worth suggesting: not in a busy worktree, not a running app's cache, not used in the last three days. */
function suggestible(e) {
  return autoOk(e) && (e.rec !== "node_modules" || e.idle_days >= 30);
}

function suggestions() {
  if (S.phase !== "ready") return [];
  const groups = new Map();
  const add = (cat, e) => (groups.get(cat) || groups.set(cat, []).get(cat)).push(e);
  for (const e of S.entries.values()) {
    const r = recOf(e);
    if (e.rec === "worktrees") {
      if (wtRemovable(e)) add("worktrees", e);
    } else if (e.rec === "trash") {
      if (!trashLocked()) add("trash", e);
    } else if (r.tier === "safe" && suggestible(e)) {
      add(r.cat, e);
    }
  }
  const out = [];
  for (const [cat, es] of groups) {
    const bytes = sum(es);
    const min = cat === "trash" ? 500e6 : 150e6;
    if (bytes < min || !SUGGEST[cat]) continue;
    const [text, action, what] = SUGGEST[cat](bytes, es.length);
    out.push({ cat, keys: es.map((e) => e.key), bytes, text, action, what });
  }
  out.sort((a, b) => (a.cat === "trash") - (b.cat === "trash") || b.bytes - a.bytes);
  // Everything that's rebuilt automatically, in one step.
  const safe = out.filter((s) => s.what);
  if (safe.length > 1) {
    const bytes = safe.reduce((a, s) => a + s.bytes, 0);
    const what = safe.map((s) => s.what);
    const list = what.slice(0, -1).join(", ") + " and " + what[what.length - 1];
    const picked = new Set(safe.flatMap((s) => s.keys));
    const rest = sum(effective(new Set([...S.entries.keys()].filter((k) => !picked.has(k) && !S.entries.get(k).parent))));
    out.unshift({ cat: "all", keys: [...picked], bytes, primary: true, action: `Clear ${fmt(bytes)}`,
      text: `<b>${fmt(bytes)}</b> is safe to clear: ${esc(list)}. Your tools rebuild it.${rest > 1e8 ? ` <span class="muted">Another ${fmt(rest)} is yours to review below.</span>` : ""}` });
  }
  return out;
}

function resultHTML(r) {
  const parts = [];
  if (r.deleted) parts.push(`Deleted ${fmt(r.deleted)}`);
  if (r.trashed) parts.push(`${parts.length ? "moved" : "Moved"} ${fmt(r.trashed)} to the Trash`);
  if (r.docker) parts.push(`${parts.length ? "pruned" : "Pruned"} Docker`);
  const n = r.errors.length;
  const bits = [];
  if (parts.length) bits.push(`<b>${esc(parts.join(", "))}.</b>`);
  if (r.stopped) bits.push("Stopped before the rest.");
  if (n) bits.push(`<span class="warn">${plural(n, "item")} couldn't be cleared.</span>`);
  if (!bits.length) bits.push("Nothing was cleared.");
  const more = [];
  if (parts.length) more.push(`${fmt(S.disk.free)} available now${r.gained > 5e6 ? `, ${fmt(r.gained)} more than before` : ""}.`);
  // The system reports far less back than was deleted: usually snapshots holding on to it.
  if (r.gained != null && r.deleted > 1e9 && r.gained < r.deleted / 2) more.push("macOS hasn't released all of it yet; snapshots can hold deleted files for a while.");
  if (r.trashed && trashLocked()) more.push("Empty the Trash from Finder to get that space back.");
  if (more.length) bits.push(`<small class="num">${esc(more.join(" "))}</small>`);
  const trash = S.entries.get("trash|.Trash");
  const buttons = [`<button type="button" class="linkbtn" data-act="log" title="A record of everything Storage has cleared">Log</button>`];
  if (n) buttons.push(`<button type="button" class="btn" data-act="issues" aria-expanded="${S.showIssues}">${S.showIssues ? "Hide details" : "Show details"}</button>`);
  if (r.trashed && trash && !trashLocked()) buttons.push(`<button type="button" class="btn" data-act="emptytrash">Empty Trash</button>`);
  buttons.push(`<button type="button" class="btn${n ? "" : " primary"}" data-act="dismiss">Done</button>`);
  const icon = parts.length ? `<span class="done">${CHECK}</span>` : "";
  return `${icon}<span class="text">${bits.join(" ")}</span>${buttons.join("")}`;
}

function renderSuggest() {
  const el = $("#suggest");
  const issues = $("#issues");
  const errs = S.result && S.showIssues && S.phase === "ready" ? S.result.errors : [];
  issues.hidden = !errs.length;
  issues.innerHTML = errs.length
    ? `<ul>${errs.map((x) => `<li><span class="in">${esc(x.name)}</span><span class="ie">${esc(x.error)}</span></li>`).join("")}</ul>`
    : "";
  delete el.dataset.keys;
  if (S.phase === "scanning" || S.phase === "idle") {
    const since = S.report ? S.report.generated : S.last ? S.last.t : 0;
    const text = S.live || !since ? "Looking for things you can clear" : `Checking what's changed since the scan ${ago(now() - since)}`;
    if (S.phase === "scanning") el.innerHTML = `${SPINNER}<span class="text">${text}</span>`;
    else if (S.intro) {
      // First launch, and macOS is hiding protected folders: say so before the scan sets off its prompts.
      el.innerHTML = `<span class="text"><b>Storage reads your home folder to measure it.</b> macOS will ask before it opens Desktop, Documents and Downloads. Without Full Disk Access it also hides the Trash, Mail and other apps' data, so those count as empty.</span>
        <button type="button" class="btn" data-act="privacy">Grant Full Disk Access</button><button type="button" class="btn primary" data-act="rescan">Scan</button>`;
    } else el.innerHTML = `<span class="text">${S.stopped ? "The scan was stopped." : "Storage hasn't scanned this Mac yet."}</span><button type="button" class="btn primary" data-act="rescan">Scan</button>`;
    return;
  }
  if (S.phase === "cleaning") {
    const stop = S.todo > 1 ? `<button type="button" class="btn keep" data-act="stop"${S.stopping ? " disabled" : ""}>${S.stopping ? "Stopping" : "Stop"}</button>` : "";
    el.innerHTML = `${SPINNER}<span class="text">Clearing <b class="num">${Math.min(S.done + 1, S.todo)}</b> of <span class="num">${S.todo}</span></span>
      <span class="meter" role="progressbar" aria-valuemin="0" aria-valuemax="${S.todo}" aria-valuenow="${S.done}"><i style="width:${(S.done / S.todo) * 100}%"></i></span>${stop}`;
    return;
  }
  if (S.phase === "error") {
    el.innerHTML = `<span class="text">The scan didn't finish.</span><button type="button" class="btn" data-act="rescan">Try again</button>`;
    return;
  }
  if (S.result) {
    el.innerHTML = resultHTML(S.result);
    return;
  }
  const list = suggestions();
  if (!list.length) {
    el.innerHTML = `<span class="done">${CHECK}</span><span class="text">Nothing else is worth clearing right now.</span>`;
    return;
  }
  S.sugIdx = Math.min(S.sugIdx, list.length - 1);
  const s = list[S.sugIdx];
  const pager = list.length > 1
    ? `<span class="pager"><button type="button" data-act="prev" aria-label="Previous suggestion">‹</button><span class="num">${S.sugIdx + 1}/${list.length}</span><button type="button" data-act="next" aria-label="Next suggestion">›</button></span>`
    : "";
  el.innerHTML = `${glyph(s.cat)}<span class="text">${s.text}</span>${pager}<button type="button" class="btn${s.primary ? " primary" : ""}" data-act="suggest">${esc(s.action)}</button>`;
  el.dataset.keys = JSON.stringify(s.keys);
}

/** A note under the overview when part of the disk couldn't be read. */
const HIDE_ACCESS_KEY = "storage.hideAccessNote";
const STARTED_KEY = "storage.started"; // a scan has been started before, so launch straight into one
const KEEP_KEY = "storage.keep";
function pref(key, value) {
  try {
    if (value === undefined) return localStorage.getItem(key);
    value === null ? localStorage.removeItem(key) : localStorage.setItem(key, value);
  } catch (_) { /* preferences are optional */ }
  return null;
}

function renderNotice() {
  const un = S.report?.unreadable || [];
  const el = $("#notice");
  el.hidden = !un.length || S.phase === "scanning" || S.hideAccess;
  if (el.hidden) return;
  const total = Math.max(un.length, S.report.unreadable_total || 0);
  const many = total.toLocaleString();
  el.innerHTML = `<span class="text"><b>${many} ${total === 1 ? "folder" : "folders"} couldn't be read</b>, so ${un.length === 1 ? "it counts" : "they count"} as empty and the totals are low. Give Storage Full Disk Access, then scan again.</span>
    <button type="button" class="btn small" data-act="privacy">Open Privacy &amp; Security</button>
    <button type="button" class="x" data-act="hideaccess" aria-label="Hide this note" title="Hide">×</button>`;
}

/* ---------- panels ---------- */

const PAGE = 100; // rows a long list shows at a time

function entryRows(entries, nested, id) {
  const sorted = entries.slice().sort((a, b) => b.size - a.size);
  const max = S.more.get(id) || PAGE;
  const more = sorted.slice(max);
  return `<ul class="paths">${sorted.slice(0, max).map((e) => {
    const name = esc(entryName(e, nested)), why = heldWhy(e), kept = S.keep.has(e.key);
    return `
      <li><label><input type="checkbox" data-key="${esc(e.key)}">
        <span><span class="pn">${name}</span><span class="pm">${esc(entryMeta(e, nested))}</span></span>
        ${why ? `<span class="tag${e.hold || kept ? " amber" : ""}">${esc(why)}</span>` : ""}
        <span class="ps num">${fmt(e.size)}</span>
        <button type="button" class="reveal${kept ? " on" : ""}" data-keep="${esc(e.key)}" aria-pressed="${kept}" title="${kept ? "Kept: never suggested or selected with its group. Click to stop keeping it." : "Keep: never suggest this or select it with its group"}" aria-label="Keep ${name}">${PIN}</button>
        <button type="button" class="reveal" data-reveal="${esc(e.key)}" title="Show in Finder" aria-label="Show ${name} in Finder">${FINDER}</button></label></li>`;
  }).join("")}
    ${more.length ? `<li class="more"><button type="button" class="linkbtn" data-more="${esc(id)}">Show ${Math.min(PAGE, more.length)} more</button><span class="num">${more.length} not shown · ${fmt(sum(more))}</span></li>` : ""}</ul>`;
}

/** Path of an entry inside its worktree (`node_modules`, `apps/web/.next`). */
function inWorktree(e) {
  const w = e.parent && S.entries.get(e.parent);
  return w && e.path.startsWith(w.path + "/") ? e.path.slice(w.path.length + 1) : null;
}

/** `nested`: shown under its worktree, so the worktree's own path is left out. */
function entryName(e, nested) {
  const inside = nested && inWorktree(e);
  if (inside) return inside;
  if (!e.label || e.label === e.path) return disp(e.path);
  if (e.rec === "node_modules" || e.rec === "venv") return disp(e.label);
  return e.label;
}

function entryMeta(e, nested) {
  const bits = [];
  if (e.failed) bits.push("couldn't be cleared; scan again for its size");
  if (nested) bits.push(recOf(e).title);
  else if (e.parent) bits.push("in a worktree");
  if (e.idle_days != null && e.idle_days >= 3) bits.push(idleText(e.idle_days));
  if (e.mtime) bits.push("modified " + monthYear(e.mtime));
  if (e.spare) bits.push(`its ${e.spare} folder is kept`);
  const where = e.cmd ? e.cmd.join(" ") : disp(e.path);
  if (!nested && where !== entryName(e)) bits.push(where);
  return bits.filter(Boolean).join("  ·  ");
}

/** A short name for messages: `repo branch` for a worktree, the entry's name otherwise. */
function entryTitle(e) {
  const w = e.worktree;
  return w ? `${repoOf(e)} ${w.branch || "(detached HEAD)"}` : entryName(e);
}
const repoOf = (e) => ((e.worktree && e.worktree.repo) || "").split("/").pop() || e.path.split("/").slice(-2, -1)[0] || "worktree";

function wtChips(e) {
  const w = e.worktree || {};
  const chips = [];
  if (S.keep.has(e.key)) chips.push(["amber", "kept"]);
  if (inUse(e)) chips.push(["blue", "in use"]);
  if (w.locked) chips.push(["amber", "locked"]);
  if (w.partial) chips.push(["amber", "can't be checked"]);
  else if (w.unknown) chips.push(["amber", "not checked"]);
  else if (w.orphaned) chips.push(["amber", "orphaned"]);
  else {
    if (w.dirty) chips.push(["amber", `${w.dirty} uncommitted`]);
    if (w.unique) chips.push(["amber", `${w.unique} unsaved ${w.unique === 1 ? "commit" : "commits"}`]);
    if (w.ignored) chips.push(["amber", `${w.ignored} ignored ${w.ignored === 1 ? "file" : "files"}`]);
    // Only "merged" is proof the work landed. A deleted upstream usually means a squash merge, not always.
    chips.push(w.merged ? ["green", "merged"] : w.gone ? ["", "upstream deleted"] : w.pushed ? ["", "pushed"] : ["", "not merged"]);
  }
  return `<span class="chips">${chips.map(([c, t]) => `<span class="chip ${c}">${esc(t)}</span>`).join("")}</span>`;
}

function wtNotes(e) {
  const w = e.worktree || {};
  const notes = [];
  const warn = (t) => notes.push(`<span class="warn">${t}</span>`);
  if (w.partial) warn("Part of this worktree's uncommitted or ignored data is unreadable, or there is too much of it to look through, so Storage can't tell whether it changes between a scan and a cleanup. It won't remove this one; if you want it gone, remove it yourself.");
  else if (w.unknown) warn("git couldn't inspect this worktree, so Storage doesn't know whether it holds uncommitted work. Look before you remove it.");
  else if (w.orphaned) warn("Its repository has no record of it any more (the repo was moved or deleted, or the worktree was pruned), so git can't say what's uncommitted. Storage hasn't checked its contents.");
  if (w.locked) warn("Locked with <code>git worktree lock</code>: someone asked for it to be kept.");
  if (w.dirty) warn(`${plural(w.dirty, "file")} with uncommitted changes. They go to the Trash with the folder, and git keeps its record of the worktree, so putting the folder back restores it, staged changes included.`);
  if (w.unique) warn(`${plural(w.unique, "commit")} on a detached HEAD that no branch contains. Storage saves a ref to them first (<code>refs/storage/trashed/…</code>), so they stay in the repo.`);
  if (w.ignored) warn(`${plural(w.ignored, "ignored file")} that git won't bring back (${esc(w.ignored_names.join(", "))}${w.ignored > w.ignored_names.length ? ", …" : ""}). ${w.ignored === 1 ? "It goes" : "They go"} to the Trash with the folder.`);
  if (inUse(e)) notes.push(w.busy ? "A process is running inside it right now." : "Touched in the last two hours, so an agent may still be working in it.");
  if (!notes.length && w.merged) notes.push("Everything here is already part of the default branch.");
  if (!notes.length && w.gone) notes.push("Its branch was deleted on the remote. That usually means the pull request was merged, but check if you're not sure.");
  if (w.branch && !w.orphaned && !w.unknown) notes.push(`The branch <code>${esc(w.branch)}</code> and its commits stay in ${esc(w.repo || "the repo")} after the folder goes.`);
  return notes.map((n) => `<p>${n}</p>`).join("");
}

const WT_FILTERS = [
  ["all", "All", () => true],
  ["finished", "Finished", (e) => wtRemovable(e)],
  ["changes", "Has work", (e) => !!(e.worktree && (e.worktree.dirty || e.worktree.unique || e.worktree.ignored))],
  ["inuse", "In use", (e) => inUse(e)],
  ["unsure", "Not checked", (e) => !!(e.worktree && (e.worktree.orphaned || e.worktree.unknown))],
];
const WT_SHOWN = 8;
const FINISHED = "Finished: git confirmed nothing is uncommitted or unsaved and no ignored file (like .env) exists only here, the work is merged or its upstream branch is gone, and nothing is using it.";

/** The last two folders of a path, which is where agents put the repo and task names. */
const tail = (p) => {
  const parts = p.split("/");
  return parts.length > 2 ? "…/" + parts.slice(-2).join("/") : disp(p);
};

function renderWorktrees() {
  const all = [...S.entries.values()].filter((e) => e.rec === "worktrees");
  $("#wtPanel").hidden = !all.length;
  if (!all.length) return;
  if (!WT_FILTERS.some(([id, , test]) => id === S.wtFilter && all.some(test))) S.wtFilter = "all";
  const test = WT_FILTERS.find(([id]) => id === S.wtFilter)[2];
  // Grouped by repository, biggest repository first, biggest worktree first inside it.
  const repos = new Map();
  for (const e of all.filter(test)) (repos.get(repoOf(e)) || repos.set(repoOf(e), []).get(repoOf(e))).push(e);
  const groups = [...repos].map(([name, es]) => ({ name, es: es.sort((a, b) => b.size - a.size), size: sum(es) })).sort((a, b) => b.size - a.size);
  const total = groups.reduce((a, g) => a + g.es.length, 0);
  let room = S.wtAll || S.wtFilter !== "all" || total <= WT_SHOWN + 2 ? Infinity : WT_SHOWN;
  const kidsOf = new Map();
  for (const k of S.entries.values()) if (k.parent) (kidsOf.get(k.parent) || kidsOf.set(k.parent, []).get(k.parent)).push(k);
  const filters = WT_FILTERS.map(([id, label, t]) => {
    const n = all.filter(t).length;
    return n || id === "all" ? `<button type="button" class="seg" data-wtf="${id}" aria-pressed="${S.wtFilter === id}">${label}<span class="num">${n}</span></button>` : "";
  }).join("");
  $("#selectMerged").hidden = !all.some((e) => wtRemovable(e));
  // What "finished" means, in sight whenever the list is narrowed to it (and on the button otherwise).
  const criteria = S.wtFilter === "finished" ? `<p class="criteria">${FINISHED}</p>` : "";
  let shown = 0, hiddenSize = 0;
  const body = groups.map((g) => {
    const rows = g.es.slice(0, room);
    room -= rows.length;
    shown += rows.length;
    hiddenSize += sum(g.es.slice(rows.length));
    if (!rows.length) return "";
    return `<div class="repo-head"><span class="rn">${esc(g.name)}</span><span class="muted num">${plural(g.es.length, "worktree")} · ${fmt(g.size)}</span></div>`
      + rows.map((e) => {
        const w = e.worktree || {};
        const open = S.open.has(e.key);
        const kids = kidsOf.get(e.key) || [];
        const name = esc(`${g.name} ${w.branch || "detached HEAD"}`);
        const age = w.last_active ? ago(now() - w.last_active) : "";
        return `<div class="item${open ? " open" : ""}" data-open="${esc(e.key)}">
        <div class="row wt">
          <input type="checkbox" data-key="${esc(e.key)}" aria-label="Select worktree ${name}">
          ${glyph("worktrees")}
          <div><div class="ti"><span class="br">${esc(w.branch || "detached HEAD")}</span></div><div class="hi" title="${esc(disp(e.path))}">${esc([w.agent || "git worktree", tail(e.path)].join("  ·  "))}</div></div>
          <span class="age num">${esc(age)}</span>
          ${wtChips(e)}
          <div class="sz num">${fmt(e.size)}</div>
          <button type="button" class="chev" aria-expanded="${open}" aria-label="Details for ${name}">${CHEV}</button>
        </div>
        ${open ? `<div class="detail">
          ${wtNotes(e)}
          <p class="where"><span class="mono">${esc(disp(e.path))}</span><button type="button" class="linkbtn" data-reveal="${esc(e.key)}">Show in Finder</button><button type="button" class="linkbtn" data-keep="${esc(e.key)}">${S.keep.has(e.key) ? "Stop keeping" : "Keep (never suggest)"}</button></p>
          ${kids.length ? `<p class="muted">Or keep the worktree and clear only what can be rebuilt:</p>${entryRows(kids, true, e.key)}` : ""}
        </div>` : ""}
      </div>`;
      }).join("");
  }).join("");
  $("#wtList").innerHTML = `<div class="group-head"><span class="segs" role="group" aria-label="Show worktrees">${filters}</span><span class="muted num">${fmt(sum(all))}</span></div>${criteria}`
    + body
    + (shown < total ? `<button type="button" class="more-row" data-act="wtall">Show all ${total} worktrees<span class="num muted">${fmt(hiddenSize)} more</span></button>` : "");
}

function matches(e, q) {
  const r = recOf(e);
  return [r.title, r.hint, e.label || "", e.path].some((t) => t.toLowerCase().includes(q));
}

/* How a group's items leave: rebuilt things are deleted, your-call things go to the Trash, and things
   a tool owns (simulators, Docker, Ollama, the Trash itself) are removed by that tool for good. */
const TIERS = [
  { id: "safe", label: "Safe to delete", sub: "rebuilt automatically · deleted right away" },
  { id: "review", label: "Review first", sub: "your call · moved to the Trash" },
  { id: "tool", label: "Removed for good", sub: "by the tool that owns them · can't be undone" },
];
const tierOf = (rec, entries) => (rec.tier === "review" && (rec.id === "trash" || entries.some((e) => e.cmd)) ? "tool" : rec.tier);

function renderRecs() {
  const q = S.filter.trim().toLowerCase();
  const recs = new Map();
  let any = false;
  for (const e of S.entries.values()) {
    if (e.rec === "worktrees") continue;
    any = true;
    if (q && !matches(e, q)) continue;
    const r = recs.get(e.rec) || recs.set(e.rec, { rec: recOf(e), entries: [] }).get(e.rec);
    r.entries.push(e);
  }
  const shown = [...recs.values()].map((r) => ({ ...r, total: sum(r.entries), tier: tierOf(r.rec, r.entries) }))
    .filter((r) => q || r.total >= 10e6).sort((a, b) => b.total - a.total);
  $("#recPanel").hidden = !any;
  $("#recList").innerHTML = TIERS.map((t) => {
    const rs = shown.filter((r) => r.tier === t.id);
    if (!rs.length) return "";
    return `<div class="group-head"><span><span class="pill ${t.id}">${t.label}</span><small>${t.sub}</small></span><span class="num muted">${fmt(rs.reduce((a, r) => a + r.total, 0))}</span></div>`
      + rs.map((r) => {
        const id = "rec:" + r.rec.id;
        const open = S.open.has(id);
        const how = r.rec.id === "trash" ? (trashLocked() ? "Storage can't read the Trash without Full Disk Access, so empty it from Finder." : "Emptying the Trash can't be undone.")
          : r.entries.some((e) => e.cmd) ? `Runs ${esc(r.entries.find((e) => e.cmd).cmd[0])} for each item.` : "";
        const held = r.entries.filter((e) => !autoOk(e));
        const why = esc([...new Set(held.map(heldWhy))].join(", "));
        const skip = !held.length ? ""
          : held.length < r.entries.length
            ? `<p class="muted">Ticking the group leaves out ${plural(held.length, "item")} (${why}). Tick ${held.length === 1 ? "it" : "them"} here to include ${held.length === 1 ? "it" : "them"}.</p>`
            : `<p class="muted">The group can't be ticked as a whole: ${held.length === 1 ? "its only item is" : `all ${held.length} items are`} held back (${why}). Tick ${held.length === 1 ? "it" : "one"} here if you mean it.</p>`;
        const count = r.entries.length > 1 ? `<span class="ct num">${r.entries.length}</span>` : "";
        return `<div class="item${open ? " open" : ""}" data-open="${id}" data-cat="${r.rec.cat}">
          <div class="row">
            <input type="checkbox" data-rec="${r.rec.id}" aria-label="Select ${esc(r.rec.title)}"${r.rec.id === "trash" && trashLocked() ? " disabled" : ""}>
            ${glyph(r.rec.cat)}
            <div><div class="ti">${esc(r.rec.title)}${count}</div><div class="hi">${esc(r.rec.hint)}</div></div>
            <span></span>
            <div class="sz num">${r.rec.approx ? "<small>up to </small>" : ""}${fmt(r.total)}</div>
            <button type="button" class="chev" aria-expanded="${open}" aria-label="Details for ${esc(r.rec.title)}">${CHEV}</button>
          </div>
          ${open ? `<div class="detail">
            <p>${esc(r.rec.desc)}${how ? " " + how : ""}</p>${skip}
            ${entryRows(r.entries, false, id)}
          </div>` : ""}
        </div>`;
      }).join("");
  }).join("") || `<p class="empty">${q ? `Nothing matches “${esc(S.filter.trim())}”.` : "Nothing worth clearing was found."}</p>`;
}

/** Entries a group checkbox stands for: the ones on screen (a filter narrows it) that aren't in use,
    held back, kept or recently used. Those can only ever be ticked one by one. */
function recEntries(id) {
  const q = S.filter.trim().toLowerCase();
  return [...S.entries.values()].filter((e) => e.rec === id && (!q || matches(e, q)) && autoOk(e));
}

function syncChecks() {
  for (const box of document.querySelectorAll("input[data-rec]")) {
    const es = recEntries(box.dataset.rec);
    const n = es.filter((e) => S.sel.has(e.key)).length;
    const others = [...S.sel].some((k) => S.entries.get(k)?.rec === box.dataset.rec);
    box.checked = n > 0 && n === es.length;
    box.indeterminate = (n > 0 && n < es.length) || (n === 0 && others);
    // Nothing in the group may be swept up: the box has nothing to stand for.
    if (!es.length) {
      box.disabled = true;
      box.title = "Everything here is in use, held back, kept or recently used. Open the group to tick items one by one.";
    }
  }
  for (const box of document.querySelectorAll("input[data-key]")) {
    const e = S.entries.get(box.dataset.key);
    box.checked = S.sel.has(box.dataset.key) || !!(e && e.parent && S.sel.has(e.parent));
    box.disabled = !!(e && e.parent && S.sel.has(e.parent)) || (e && e.rec === "trash" && trashLocked()) || uncheckable(e);
    if (uncheckable(e)) box.title = "Storage can't check this worktree for changes, so it won't remove it.";
  }
  const eff = effective(S.sel);
  const on = eff.length > 0 && S.phase === "ready";
  $("#selbar").hidden = !on;
  document.body.classList.toggle("has-sel", on);
  $("#selSize").textContent = fmt(sum(eff));
  $("#selCount").textContent = "selected · " + plural(eff.length, "item");
}

function renderPanels() {
  $("#skeleton").hidden = !(S.phase === "scanning" && !S.entries.size);
  renderNotice();
  renderWorktrees();
  renderRecs();
  syncChecks();
  renderExplorer();
}

function renderLive() {
  renderHeader();
  renderStatus();
  renderLegend();
  bar && bar.wake();
}

function renderAll() {
  document.body.classList.toggle("busy", isBusy());
  // While a scan or cleanup runs the lists are for reading only, for the keyboard as much as the pointer.
  $("#cleanView").inert = isBusy();
  renderLive();
  renderSuggest();
  renderPanels();
}

/* ---------- confirm & clean ---------- */

let pendingKeys = null;

/* The last look before anything goes: every item by name under the way it leaves, and every risk
   Storage knows about with the items it applies to. */
function confirmClear(keys) {
  const picked = new Set(keys);
  const es = effective(picked);
  if (!es.length) return;
  pendingKeys = es.map((e) => e.key);
  const by = { delete: [], trash: [], command: [], empty: [] };
  for (const e of es) by[methodOf(e)].push(e);
  const names = (list) => list.slice(0, 3).map(entryTitle).join(", ") + (list.length > 3 ? ` and ${list.length - 3} more` : "");
  const is = (list, one, many) => (list.length === 1 ? one : many);
  const wts = es.filter((e) => e.rec === "worktrees" && e.worktree);

  const warns = [];
  const warn = (list, text) => list.length && warns.push(text(list));
  warn(wts.filter((e) => e.worktree.unknown || e.worktree.orphaned), (l) => `${plural(l.length, "worktree")} ${is(l, "wasn't", "weren't")} inspected by git (${names(l)}), so Storage doesn't know what ${is(l, "it holds", "they hold")}.`);
  warn(wts.filter((e) => e.worktree.dirty), (l) => `${plural(l.length, "worktree")} ${is(l, "has", "have")} uncommitted changes (${names(l)}). They go to the Trash with the folder.`);
  warn(wts.filter((e) => e.worktree.unique), (l) => `${plural(l.length, "worktree")} ${is(l, "has", "have")} commits no branch contains (${names(l)}). A ref is saved under refs/storage/trashed first, so they stay in the repo.`);
  warn(wts.filter((e) => e.worktree.ignored), (l) => `${plural(l.length, "worktree")} ${is(l, "holds", "hold")} ignored files git won't bring back, like .env (${names(l)}).`);
  warn(wts.filter((e) => e.worktree.locked), (l) => `${plural(l.length, "worktree")} ${is(l, "is", "are")} locked with git worktree lock (${names(l)}).`);
  warn(es.filter(inUse), (l) => `${plural(l.length, "item")} ${is(l, "is", "are")} in a worktree that's in use (${names(l)}). An agent may still be working there.`);
  warn(es.filter((e) => e.hold === "app is running"), (l) => `${plural(l.length, "cache")} ${is(l, "belongs to an app that is", "belong to apps that are")} running (${names(l)}). Quit ${is(l, "it", "them")} first, or ${is(l, "it", "they")} may misbehave until restarted.`);
  warn(es.filter((e) => e.hold === "default version"), (l) => `${names(l)}: ${is(l, "this is the default version", "these are the default versions")}. Removing ${is(l, "it", "them")} can break your shell or the tools installed under ${is(l, "it", "them")}.`);
  warn(es.filter((e) => e.hold === "booted"), (l) => `${plural(l.length, "simulator")} ${is(l, "is", "are")} booted (${names(l)}) and won't be deleted until shut down.`);
  warn(es.filter((e) => e.hold && !["app is running", "default version", "booted"].includes(e.hold)), (l) => `${names(l)}: ${l[0].hold}.`);
  warn(es.filter((e) => S.keep.has(e.key)), (l) => `You asked to keep ${names(l)}.`);
  warn(by.command, (l) => `${plural(l.length, "item")} ${is(l, "is", "are")} removed by ${is(l, "its", "their")} own tool and can't be put back (${names(l)}).`);
  warn(by.empty, () => "Emptying the Trash can't be undone. It is emptied before anything else is moved into it.");

  const row = (e) => {
    const title = entryTitle(e);
    const sub = e.cmd ? e.cmd.join(" ") : e.rec === "trash" ? "" : e.worktree ? disp(e.path) : [recOf(e).title, disp(e.path) !== title ? disp(e.path) : ""].filter(Boolean).join("  ·  ");
    const inside = e.rec === "worktrees" ? [...picked].filter((k) => S.entries.get(k)?.parent === e.key).length : 0;
    return `<li title="${esc(title)}${sub ? "\n" + esc(sub) : ""}"><span class="cn"><span>${esc(title)}</span><small>${esc(sub)}${inside ? `${sub ? "  ·  " : ""}includes the ${plural(inside, "item")} you ticked inside it` : ""}</small></span><span class="num">${e.rec === "docker" ? "up to " : ""}${fmt(e.size)}</span></li>`;
  };
  const section = (title, sub, list) => (list.length
    ? `<section class="cf-group"><h3><span>${title}<small>${sub}</small></span><span class="num">${plural(list.length, "item")} · ${fmt(sum(list))}</span></h3><ul>${list.slice().sort((a, b) => b.size - a.size).map(row).join("")}</ul></section>`
    : "");
  const permanent = by.delete.length + by.command.length + by.empty.length > 0;
  $("#confirmSize").textContent = fmt(sum(es));
  $("#confirmWarn").innerHTML = warns.map((w) => `<p class="cf-warn">${esc(w)}</p>`).join("");
  $("#confirmBody").innerHTML =
    section("Emptied", "everything in the Trash, for good", by.empty)
    + section("Removed by their own tool", "for good", by.command)
    + section("Deleted", "right away; your tools rebuild these", by.delete)
    + section("Moved to the Trash", "put back from the Trash until you empty it", by.trash);
  $("#confirmOk").textContent = permanent ? `Clear ${fmt(sum(es))}` : "Move to Trash";
  $("#confirmOk").classList.toggle("danger", permanent);
  $("#confirmOk").classList.toggle("primary", !permanent);
  $("#confirm").returnValue = ""; // Escape keeps the previous value otherwise
  $("#confirm").showModal();
  $("#confirmBody").scrollTop = 0;
}

function verbFor(e) {
  switch (methodOf(e)) {
    case "trash": return "Moving to the Trash";
    case "command": return "Running " + e.cmd.slice(0, 2).join(" ");
    case "empty": return "Emptying the Trash";
    default: return "Clearing " + recOf(e).title.toLowerCase();
  }
}

/** Updates the model as if `e` were gone, and animates it away. */
function removeEntry(e, method) {
  const docker = e.rec === "docker"; // the disk image doesn't shrink until Docker compacts it
  if (!docker) {
    bar.dissolve(e.cat, e.size);
    S.cats[e.cat] = Math.max(0, (S.cats[e.cat] || 0) - e.size);
    treeAdjust(e.path, -e.size);
    if (method === "trash") {
      treeAdjust(".Trash", e.size);
      S.cats.trash = (S.cats.trash || 0) + e.size;
      const t = S.entries.get("trash|.Trash");
      if (t) t.size += e.size;
      else S.entries.set("trash|.Trash", { key: "trash|.Trash", rec: "trash", path: ".Trash", size: e.size, cat: "trash" });
    } else {
      S.freed += e.size;
    }
  }
  S.entries.delete(e.key);
  for (const k of [...S.entries.values()]) if (k.parent === e.key) S.entries.delete(k.key);
  const parent = e.parent && S.entries.get(e.parent);
  if (parent) parent.size = Math.max(0, parent.size - e.size);
  S.sel.delete(e.key);
}

async function runClean(keys) {
  Object.assign(S, { phase: "cleaning", result: null, showIssues: false, hover: null, done: 0, todo: keys.length, stopping: false });
  renderAll();
  const res = { deleted: 0, trashed: 0, docker: false, errors: [], stopped: false, gained: null };
  let before = null;
  try {
    before = (await invoke("disk")).free;
  } catch (_) { /* the gain just isn't reported */ }
  const ch = new TAURI.core.Channel();
  ch.onmessage = (ev) => {
    const e = S.entries.get(ev.key);
    if (!e) return;
    if (ev.kind === "start") {
      S.current = { verb: verbFor(e), path: e.cmd ? e.label || disp(e.path) : disp(e.path) };
      bar.spawn(e.cat, ...edgeOf(e.cat), 10);
    } else if (ev.kind === "done") {
      if (ev.method === "trash") res.trashed += e.size;
      else if (e.rec === "docker") res.docker = true;
      else res.deleted += e.size;
      S.done++;
      removeEntry(e, ev.method);
    } else {
      S.done++;
      e.failed = true; // part of it may be gone, so its size is no longer a measurement
      // The backend's message starts with the path, which the row already shows.
      res.errors.push({ name: entryTitle(e), error: String(ev.error).replace(disp(e.path) + ": ", "") });
    }
    renderLive();
    renderSuggest();
  };
  try {
    const summary = await invoke("clean", { keys, onProgress: ch });
    res.stopped = !!summary.stopped && S.done < S.todo;
    // From here the numbers are the system's, not sums of what was cleared: hard links, clones
    // and snapshots all make the two differ.
    if (summary.disk && summary.disk.total) {
      if (before != null) res.gained = summary.disk.free - before;
      S.disk = summary.disk;
      S.usedAtScan = S.disk.total - S.disk.free;
      S.freed = 0;
      S.system = Math.max(0, S.usedAtScan - Object.values(S.cats).reduce((a, b) => a + b, 0));
    }
  } catch (err) {
    res.errors.push({ name: "Cleanup", error: String(err) });
  }
  S.sel.clear(); // what failed stays listed with its reason; nothing stays armed for another click
  rememberNow();
  Object.assign(S, { current: null, phase: "ready", result: res, sugIdx: 0 });
  renderAll();
}

function edgeOf(id) {
  const s = bar.spans().find((x) => x.id === id);
  return s ? [Math.max(s.x0, s.x1 - PITCH * 3), s.x1] : [bar.bar.x, bar.bar.x];
}

/* ---------- scan ---------- */

const CANCELLED = "cancelled"; // what the backend's `scan` fails with after `cancel`

function applyReport(r) {
  S.report = r;
  S.disk = r.disk;
  S.cats = { ...r.cats };
  S.scanned = r.scanned;
  S.freed = 0;
  S.usedAtScan = r.disk.total - r.disk.free;
  S.system = Math.max(0, S.usedAtScan - r.scanned);
  S.recs = new Map(r.recs.map((x) => [x.id, x]));
  S.entries = new Map(r.entries.map((e) => [e.key, e]));
  // A new report is a new thing to review: nothing ticked under the old one carries over, whether
  // or not an entry with the same key is still there (it may be in use now, or no longer checkable).
  S.sel.clear();
  const at = EX.view && EX.view.path;
  indexTree(r.tree);
  if (at != null && EX.byPath.has(at)) EX.view = EX.byPath.get(at);
  saveLast(r);
}

/* A rescan keeps the current results on screen (dimmed) until the new ones arrive, and the first
   scan after a launch starts from the last scan's totals, so there's always something to look at. */
async function scan() {
  if (isBusy()) return;
  const had = !!S.report;
  pref(STARTED_KEY, "1");
  Object.assign(S, { phase: "scanning", intro: false, error: "", result: null, showIssues: false, hover: null, progress: 0, files: 0, scanPath: "", stopped: false });
  S.live = !had && !S.last;
  // At launch the last scan's list is shown, dimmed and inert, until this scan replaces it.
  const remembered = !had && S.recs.size && S.last && Array.isArray(S.last.entries) ? S.last.entries : null;
  if (!had) {
    S.cats = S.last ? { ...S.last.cats } : {};
    S.entries = new Map((remembered || []).filter((e) => S.recs.has(e.rec)).map((e) => [e.key, e]));
    S.freed = 0;
    S.system = 0;
  }
  renderAll();
  try {
    S.disk = await invoke("disk");
    // The headline is what the system says is used, from the first moment. Only the bar fills in.
    S.usedAtScan = S.disk.total - S.disk.free;
    if (!had && S.last) S.system = Math.max(0, S.usedAtScan - Object.values(S.cats).reduce((a, b) => a + b, 0));
  } catch (_) { /* shown once the scan returns */ }
  renderLive();
  const expected = (S.report || S.last || {}).files || 0;
  let listed = 0;
  const ch = new TAURI.core.Channel();
  ch.onmessage = (p) => {
    if (S.phase !== "scanning") return;
    S.files = p.files;
    S.scanPath = p.path;
    if (S.live) S.cats = p.cats;
    else S.progress = Math.min(0.99, p.files / expected);
    // On a first scan the list fills in as things are found. A rescan keeps showing the last results.
    if (!had && !remembered && S.recs.size && p.entries && p.entries.length) {
      for (const e of p.entries) S.entries.set(e.key, e);
      // Not on every message: a redraw per second is plenty while nothing can be clicked yet.
      if (performance.now() - listed > 900) {
        listed = performance.now();
        renderPanels();
      }
    }
    renderLive();
  };
  try {
    applyReport(await invoke("scan", { onProgress: ch }));
    S.phase = "ready";
  } catch (err) {
    if (String(err) === CANCELLED) {
      S.phase = had ? "ready" : "idle";
      S.stopped = true;
    } else {
      S.phase = "error";
      S.error = String(err);
    }
    if (!had) {
      // Nothing listed during a scan that didn't finish can be acted on.
      S.entries = new Map();
      if (S.live) S.cats = {};
    }
  }
  S.live = false;
  S.progress = 0;
  renderAll();
}

/* ---------- explorer (drill-down) ---------- */

/* A squarified treemap of the scan tree: two levels at a time, click a folder to go in, Escape to go
   back. Tiles that are cleanup entries get a checkbox that shares the selection with the lists. */

const EX = { root: null, view: null, byPath: new Map(), tiles: [], entries: new Map() };

function indexTree(root) {
  EX.root = root;
  EX.view = root;
  EX.byPath = new Map();
  (function walk(n, parent) {
    n.parent = parent;
    // Root folders carry their path; deeper ones are built from names. Aggregates and virtual nodes have none.
    n.path = !parent ? null : n.o || n.x ? null : n.p != null ? n.p : parent.path == null ? null : parent.path === "" ? n.n : parent.path + "/" + n.n;
    if (n.path != null) EX.byPath.set(n.path, n);
    for (const k of n.c || []) walk(k, n);
  })(root, null);
}

const nodeLabel = (n) => (n.o ? n.n : n.n || "?");
const canOpen = (n) => !!(n.c && n.c.length && !n.o);
const attached = (n) => {
  for (; n && n.parent; n = n.parent) if (!n.parent.c || !n.parent.c.includes(n)) return false;
  return !!n;
};

/** Shrinks (or grows) the tree for a path that was cleared or moved, at the deepest folder the tree still has. */
function treeAdjust(path, delta) {
  if (!EX.root || path == null) return;
  let p = path;
  let n = EX.byPath.get(p);
  while (!n) {
    const i = p.lastIndexOf("/");
    if (i <= 0) {
      n = p.startsWith("/") ? null : EX.byPath.get("");
      break;
    }
    p = p.slice(0, i);
    n = EX.byPath.get(p);
  }
  if (!n) return;
  let from = n;
  if (n.path === path && delta < 0 && n.parent) {
    n.parent.c = n.parent.c.filter((k) => k !== n);
    EX.byPath.delete(path);
    from = n.parent;
  }
  for (let a = from; a; a = a.parent) a.s = Math.max(0, a.s + delta);
  while (EX.view.parent && !attached(EX.view)) EX.view = EX.view.parent;
}

function squarify(values, x, y, w, h) {
  const out = values.map(() => ({ x, y, w: 0, h: 0 }));
  const total = values.reduce((a, b) => a + b, 0);
  if (!total || w <= 0 || h <= 0) return out;
  const order = values.map((_, i) => i).sort((a, b) => values[b] - values[a]);
  const areas = order.map((i) => (values[i] * w * h) / total);
  let rx = x, ry = y, rw = w, rh = h, row = [];
  const worst = (r, side) => {
    let s = 0, mx = 0, mn = Infinity;
    for (const a of r) { s += a; mx = Math.max(mx, a); mn = Math.min(mn, a); }
    return Math.max((side * side * mx) / (s * s), (s * s) / (side * side * mn));
  };
  const flush = () => {
    const s = row.reduce((a, j) => a + areas[j], 0);
    if (rw >= rh) {
      const cw = s / rh;
      let cy = ry;
      for (const j of row) { const hh = areas[j] / cw; out[order[j]] = { x: rx, y: cy, w: cw, h: hh }; cy += hh; }
      rx += cw; rw -= cw;
    } else {
      const ch = s / rw;
      let cx = rx;
      for (const j of row) { const ww = areas[j] / ch; out[order[j]] = { x: cx, y: ry, w: ww, h: ch }; cx += ww; }
      ry += ch; rh -= ch;
    }
    row = [];
  };
  let i = 0;
  while (i < areas.length) {
    if (areas[i] <= 0) { i++; continue; }
    const side = Math.min(rw, rh);
    const cur = row.map((j) => areas[j]);
    if (!row.length || worst([...cur, areas[i]], side) <= worst(cur, side)) row.push(i++);
    else flush();
  }
  if (row.length) flush();
  return out;
}
const inset = (r, d) => ({ x: r.x + d, y: r.y + d, w: r.w - 2 * d, h: r.h - 2 * d });

/** Skips through folders that hold almost nothing but one subfolder (.ollama → models → blobs). */
function unwrap(node) {
  while (node.c && node.c[0] && node.c[0].c && !node.c[0].o && node.c[0].s >= node.s * 0.9) node = node.c[0];
  return node;
}

function entriesByPath() {
  const m = new Map();
  for (const e of S.entries.values()) (m.get(e.path) || m.set(e.path, []).get(e.path)).push(e);
  return m;
}

/** The entry for this exact path, or the nearest one covering it from above. */
function coveringEntry(path, byPath) {
  if (path == null) return null;
  for (let p = path; ; ) {
    const es = byPath.get(p);
    if (es) return { entry: es[0], exact: p === path };
    const i = p.lastIndexOf("/");
    if (i <= 0) return null;
    p = p.slice(0, i);
  }
}

function tileHTML(node, r, level, nest, cov) {
  const name = esc(nodeLabel(node));
  const box = cov && cov.exact && !uncheckable(cov.entry) && S.phase === "ready" && r.w >= 70 && r.h >= 40
    ? `<input type="checkbox" class="tchk" data-key="${esc(cov.entry.key)}" aria-label="Select ${name} for cleanup"${S.sel.has(cov.entry.key) ? " checked" : ""}>`
    : "";
  if (nest) return `<div class="th"><span class="tn">${name}</span><span class="ts num">${fmt(node.s)}</span>${box}</div>`;
  if (r.w >= 64 && r.h >= 48 && !node.o) {
    const fs = Math.max(12, Math.min(24, Math.sqrt(r.w * r.h) / 9));
    return `<div class="leaf"><span class="tn">${name}</span><span class="big num" style="font-size:${fs.toFixed(1)}px">${fmt(node.s)}</span></div>${box}`;
  }
  if (r.w >= 44 && r.h >= 24) return `<div class="leaf"><span class="tn">${name}</span></div>${box}`;
  return "";
}

function drawTile(parent, node, r, level, byPath) {
  if (r.w < 4 || r.h < 4) return;
  const el = document.createElement("div");
  el.dataset.i = EX.tiles.push(node) - 1;
  const radius = Math.max(2, Math.min(level ? 6 : 9, Math.min(r.w, r.h) / 4));
  el.style.cssText = `left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;border-radius:${radius.toFixed(1)}px;--c:${(CAT[node.t] || CAT.other).color}`;
  const inner = level === 0 ? unwrap(node) : node;
  const nest = level === 0 && canOpen(inner) && r.w >= 120 && r.h >= 96;
  const cov = coveringEntry(node.path, byPath);
  const cls = ["tile"];
  if (level) cls.push("in");
  if (node.o) cls.push("agg");
  if (node.x) cls.push("virt");
  if (canOpen(node)) cls.push("can-open");
  if (cov) cls.push(S.sel.has(cov.entry.key) || (cov.entry.parent && S.sel.has(cov.entry.parent)) ? "sel" : "clear");
  if (nest) cls.push("group");
  el.className = cls.join(" ");
  el.innerHTML = tileHTML(node, r, level, nest, cov);
  if (nest) {
    const kids = inner.c.filter((k) => k.s > 0);
    const rects = squarify(kids.map((k) => k.s), 6, 29, r.w - 14, r.h - 37);
    kids.forEach((k, i) => drawTile(el, k, inset(rects[i], 2), 1, byPath));
  }
  // Every tile can be reached from the keyboard: to open it (or the folder it sits in) and to read its details.
  {
    el.tabIndex = 0;
    el.setAttribute("role", "button");
    el.setAttribute("aria-label", `${canOpen(node) ? "Open " : ""}${nodeLabel(node)}, ${fmt(node.s)}`);
  }
  parent.appendChild(el);
}

function renderExplorer() {
  if ($("#exploreView").hidden) return;
  const tm = $("#tm");
  EX.tiles = [];
  tm.replaceChildren();
  $("#exploreView").classList.toggle("pending", !EX.root);
  if (!EX.root) {
    // Nothing to map until a scan has finished; no path to reveal either.
    tm.innerHTML = `<div class="tm-empty">${S.phase === "scanning" ? "The map appears when the scan finishes." : "Scan to see where the space went."}</div>`;
    $("#crumbs").innerHTML = "";
    $("#viewSize").textContent = "";
    $("#revealView").hidden = true;
    $("#mapNote").textContent = "";
    return;
  }
  // Space that was never walked has nothing inside to open, so it gets a line of text, not half the map.
  const all = (EX.view.c || []).filter((k) => k.s > 0);
  const unmapped = all.filter((k) => k.x && !k.c);
  const kids = all.filter((k) => !unmapped.includes(k));
  EX.entries = entriesByPath();
  if (!kids.length) {
    tm.innerHTML = '<div class="tm-empty">Nothing here is larger than 8 MB.</div>';
  } else {
    const rects = squarify(kids.map((k) => k.s), 0, 0, tm.clientWidth, tm.clientHeight);
    kids.forEach((k, i) => drawTile(tm, k, inset(rects[i], 3), 0, EX.entries));
  }
  $("#mapNote").textContent = unmapped.length
    ? `Not on the map: ${fmt(unmapped.reduce((a, k) => a + k.s, 0))} of macOS, other apps' data and anything Storage can't read.`
    : "";
  const chain = [];
  for (let n = EX.view; n; n = n.parent) chain.unshift(n);
  $("#crumbs").innerHTML = chain.map((n, i) =>
    (i ? '<span class="sep">/</span>' : "") +
    `<button type="button" data-depth="${i}"${i === chain.length - 1 ? ' aria-current="page"' : ""}>${esc(nodeLabel(n))}</button>`).join("");
  $("#viewSize").textContent = fmt(EX.view.s);
  $("#revealView").hidden = EX.view.path == null;
}

function openNode(node) {
  if (!node || !canOpen(node)) return;
  EX.view = node;
  hideTip();
  renderExplorer();
}

const tip = () => $("#tip");
function showTip(node, x, y) {
  const parent = node.parent;
  const pct = parent && parent.s ? Math.round((node.s / parent.s) * 1000) / 10 : 100;
  const cov = coveringEntry(node.path, EX.entries);
  let rec = "";
  if (cov) {
    const r = recOf(cov.entry);
    const state = S.sel.has(cov.entry.key) ? "selected" : r.tier === "safe" ? "safe to delete" : "review first";
    rec = `${esc(r.title)}: ${state}`;
  }
  const where = node.path != null ? disp(node.path) : node.o ? `${node.o.toLocaleString()} smaller items, grouped so the map stays readable` : node.x ? "measured by macOS, not a folder you can open" : "";
  const el = tip();
  el.innerHTML = `<div><b>${esc(nodeLabel(node))}</b> ${fmt(node.s)} <span class="dim">(${pct}% of ${esc(parent ? nodeLabel(parent) : "disk")})</span></div>`
    + (where ? `<div class="p">${esc(where)}</div>` : "")
    + `<div class="r">${esc((CAT[node.t] || CAT.other).name)}${rec ? "<br>" + rec : ""}</div>`
    + (node.path != null ? '<div class="k">⌥-click to show in Finder</div>' : "");
  el.hidden = false;
  const b = el.getBoundingClientRect();
  el.style.left = Math.min(x + 14, innerWidth - b.width - 8) + "px";
  el.style.top = (y + 18 + b.height > innerHeight ? y - b.height - 10 : y + 18) + "px";
}
function hideTip() {
  tip().hidden = true;
}

function setTab(tab) {
  S.tab = tab;
  for (const b of document.querySelectorAll(".tabs button")) {
    b.setAttribute("aria-selected", b.dataset.tab === tab);
    b.tabIndex = b.dataset.tab === tab ? 0 : -1;
  }
  $("#cleanView").hidden = tab !== "clean";
  $("#exploreView").hidden = tab !== "explore";
  renderExplorer();
  // The map wants the whole window: bring the tabs to the top so it fits below them.
  if (tab === "explore") $(".tabs").scrollIntoView({ block: "start", behavior: REDUCED.matches ? "auto" : "smooth" });
}

function wireExplorer() {
  $(".tabs").addEventListener("click", (ev) => {
    const b = ev.target.closest("button[data-tab]");
    if (b) setTab(b.dataset.tab);
  });
  $(".tabs").addEventListener("keydown", (ev) => {
    if (ev.key !== "ArrowLeft" && ev.key !== "ArrowRight") return;
    ev.preventDefault();
    setTab(S.tab === "clean" ? "explore" : "clean");
    $(`.tabs button[data-tab="${S.tab}"]`).focus();
  });
  $("#crumbs").addEventListener("click", (ev) => {
    const b = ev.target.closest("button[data-depth]");
    if (!b) return;
    const chain = [];
    for (let n = EX.view; n; n = n.parent) chain.unshift(n);
    openNode(chain[+b.dataset.depth]);
  });
  $("#revealView").addEventListener("click", () => {
    if (EX.view && EX.view.path != null) invoke("reveal_path", { path: EX.view.path }).catch(() => {});
  });
  const tm = $("#tm");
  tm.addEventListener("click", (ev) => {
    if (ev.target.matches(".tchk")) return; // handled by the shared change listener
    const el = ev.target.closest(".tile");
    if (!el) return;
    const node = EX.tiles[el.dataset.i];
    if (ev.altKey) {
      if (node.path != null) invoke("reveal_path", { path: node.path }).catch(() => {});
      return;
    }
    if (canOpen(node)) return openNode(node);
    const outer = el.parentElement.closest(".tile");
    if (outer) openNode(EX.tiles[outer.dataset.i]);
  });
  tm.addEventListener("keydown", (ev) => {
    if ((ev.key === "Enter" || ev.key === " ") && ev.target.classList.contains("tile")) {
      ev.preventDefault();
      const node = EX.tiles[ev.target.dataset.i];
      // A tile that can't be opened itself opens the folder it sits in.
      const outer = ev.target.parentElement.closest(".tile");
      openNode(canOpen(node) ? node : outer ? EX.tiles[outer.dataset.i] : null);
      tm.querySelector(".tile[tabindex]")?.focus();
    }
  });
  // The same details a pointer gets on hover, for whoever arrives by keyboard.
  tm.addEventListener("focusin", (ev) => {
    if (!ev.target.classList.contains("tile") || !ev.target.matches(":focus-visible")) return;
    const r = ev.target.getBoundingClientRect();
    showTip(EX.tiles[ev.target.dataset.i], r.left + 8, r.top + 8);
  });
  tm.addEventListener("focusout", hideTip);
  tm.addEventListener("mousemove", (ev) => {
    const el = ev.target.closest(".tile");
    el ? showTip(EX.tiles[el.dataset.i], ev.clientX, ev.clientY) : hideTip();
  });
  tm.addEventListener("mouseleave", hideTip);
  document.addEventListener("keydown", (ev) => {
    if ((ev.key === "Escape" || ev.key === "Backspace") && S.tab === "explore" && EX.view?.parent
        && !$("#confirm").open && !/INPUT|TEXTAREA/.test(document.activeElement.tagName)) {
      ev.preventDefault();
      openNode(EX.view.parent);
    }
  });
  let raf = 0;
  new ResizeObserver(() => {
    cancelAnimationFrame(raf);
    raf = requestAnimationFrame(renderExplorer);
  }).observe(tm);
}

/* ---------- events ---------- */

let bar;

function setHover(id) {
  if (S.hover === id) return;
  S.hover = id;
  renderStatus();
  renderLegend();
  bar.wake();
}

function stop() {
  if (S.phase === "cleaning") S.stopping = true;
  invoke("cancel").catch(() => {});
  renderSuggest();
}

function toggle(keys, on) {
  for (const k of keys) on && !uncheckable(S.entries.get(k)) ? S.sel.add(k) : S.sel.delete(k);
  syncChecks();
  renderExplorer();
}

function wire() {
  const hero = $("#hero");
  hero.addEventListener("mouseenter", () => bar.wake()); // the shimmer comes back while it's being looked at
  hero.addEventListener("mousemove", (ev) => {
    const lg = ev.target.closest(".lg");
    setHover(lg ? lg.dataset.cat : bar.segmentAt(ev.clientX, ev.clientY));
  });
  hero.addEventListener("mouseleave", () => setHover(null));

  $("#legend").addEventListener("click", (ev) => {
    const lg = ev.target.closest(".lg");
    if (!lg) return;
    const id = lg.dataset.cat;
    if (S.tab !== "clean") setTab("clean");
    const target = id === "worktrees" ? $("#wtPanel") : document.querySelector(`#recList .item[data-cat="${id}"]`);
    if (target && !target.hidden) {
      if (target.classList.contains("item") && !target.classList.contains("open")) toggleOpen(target);
      target.scrollIntoView({ behavior: "smooth", block: "center" });
      target.classList.add("hot");
      setTimeout(() => target.classList.remove("hot"), 1200);
    }
  });

  $("#suggest").addEventListener("click", (ev) => {
    const b = ev.target.closest("button[data-act]");
    if (!b) return;
    const act = b.dataset.act;
    const n = suggestions().length;
    if (act === "next") S.sugIdx = (S.sugIdx + 1) % n;
    else if (act === "prev") S.sugIdx = (S.sugIdx - 1 + n) % n;
    else if (act === "suggest") return confirmClear(JSON.parse($("#suggest").dataset.keys || "[]"));
    else if (act === "rescan") return scan();
    else if (act === "stop") return stop();
    else if (act === "issues") S.showIssues = !S.showIssues;
    else if (act === "dismiss") S.result = null;
    else if (act === "emptytrash") return confirmClear(["trash|.Trash"]);
    else if (act === "log") return void invoke("reveal_log").catch(() => {});
    else if (act === "privacy") return; // the document handler opens Settings
    renderSuggest();
    // Paging shouldn't drop keyboard focus when the row is redrawn.
    if (act === "next" || act === "prev" || act === "issues") $(`#suggest [data-act="${act}"]`)?.focus();
  });

  $("#rescan").addEventListener("click", () => (S.phase === "scanning" ? stop() : scan()));

  $("#wtList").addEventListener("click", (ev) => {
    const f = ev.target.closest("[data-wtf]");
    if (f) S.wtFilter = f.dataset.wtf;
    else if (ev.target.closest('[data-act="wtall"]')) S.wtAll = true;
    else return;
    renderWorktrees();
    syncChecks();
    if (f) $(`#wtList [data-wtf="${S.wtFilter}"]`)?.focus();
  });

  $("#filter").addEventListener("input", (ev) => {
    S.filter = ev.target.value;
    renderRecs();
    syncChecks();
  });

  document.addEventListener("change", (ev) => {
    const t = ev.target;
    if (t.dataset.rec) {
      const es = recEntries(t.dataset.rec);
      toggle(es.map((e) => e.key), !es.every((e) => S.sel.has(e.key)));
    } else if (t.dataset.key) {
      toggle([t.dataset.key], t.checked);
    }
  });

  document.addEventListener("click", (ev) => {
    const keep = ev.target.closest("[data-keep]");
    if (keep) {
      ev.preventDefault();
      const k = keep.dataset.keep;
      S.keep.has(k) ? S.keep.delete(k) : S.keep.add(k);
      if (S.keep.has(k)) S.sel.delete(k);
      pref(KEEP_KEY, JSON.stringify([...S.keep]));
      renderSuggest();
      renderWorktrees();
      renderRecs();
      syncChecks();
      document.querySelector(`[data-keep="${CSS.escape(k)}"]`)?.focus();
      return;
    }
    const more = ev.target.closest("[data-more]");
    if (more) {
      ev.preventDefault();
      const id = more.dataset.more;
      S.more.set(id, (S.more.get(id) || PAGE) + PAGE);
      id.startsWith("rec:") ? renderRecs() : renderWorktrees();
      syncChecks();
      return;
    }
    const reveal = ev.target.closest("[data-reveal]");
    if (reveal) {
      ev.preventDefault();
      invoke("reveal", { key: reveal.dataset.reveal }).catch(() => {});
      return;
    }
    if (ev.target.closest('[data-act="privacy"]')) {
      invoke("open_privacy_settings").catch(() => {});
      return;
    }
    const access = ev.target.closest('[data-act="hideaccess"], [data-act="showaccess"]');
    if (access) {
      S.hideAccess = access.dataset.act === "hideaccess";
      pref(HIDE_ACCESS_KEY, S.hideAccess ? "1" : null);
      renderNotice();
      renderStatus();
      return;
    }
    const row = ev.target.closest(".row");
    if (row && !ev.target.matches("input")) toggleOpen(row.parentElement);
  });

  $("#selectMerged").title = FINISHED;
  $("#selectMerged").addEventListener("click", () => {
    const keys = [...S.entries.values()].filter((e) => e.rec === "worktrees" && wtRemovable(e)).map((e) => e.key);
    // Show exactly what was picked and why, not a few ticks scattered through a long list.
    S.wtFilter = "finished";
    renderWorktrees();
    toggle(keys, true);
  });
  $("#clearSel").addEventListener("click", () => toggle([...S.sel], false));
  $("#reviewBtn").addEventListener("click", () => confirmClear([...S.sel]));
  $("#confirm").addEventListener("close", () => {
    const keys = pendingKeys;
    pendingKeys = null;
    if ($("#confirm").returnValue === "ok" && keys) runClean(keys);
  });

  document.addEventListener("keydown", (ev) => {
    const cmd = ev.metaKey || ev.ctrlKey;
    if (cmd && ev.key === "r") {
      ev.preventDefault();
      if (!$("#confirm").open) scan();
    } else if (cmd && ev.key === "f" && !$("#recPanel").hidden) {
      ev.preventDefault();
      if (S.tab !== "clean") setTab("clean");
      $("#filter").focus();
      $("#filter").select();
    } else if (ev.key === "Escape" && ev.target.id === "filter" && S.filter) {
      ev.target.value = S.filter = "";
      renderRecs();
      syncChecks();
    } else if ((ev.key === "Escape" || (cmd && ev.key === ".")) && isBusy() && !(S.tab === "explore" && EX.view?.parent)) {
      stop();
    }
  });
}

/** Details are only built for open rows, so opening one redraws its list. */
function toggleOpen(item) {
  const id = item.dataset.open;
  if (!id) return;
  S.open.has(id) ? S.open.delete(id) : S.open.add(id);
  const focused = item.contains(document.activeElement);
  id.startsWith("rec:") ? renderRecs() : renderWorktrees();
  syncChecks();
  if (focused) document.querySelector(`[data-open="${CSS.escape(id)}"] .chev`)?.focus();
}

/* ---------- init ---------- */

async function start() {
  S.last = loadLast();
  S.hideAccess = pref(HIDE_ACCESS_KEY) === "1";
  try {
    S.keep = new Set(JSON.parse(pref(KEEP_KEY) || "[]"));
  } catch (_) { /* start with nothing kept */ }
  renderLegend();
  bar = new DotBar($("#hero"), $("#barSlot"), $("#fx"));
  wire();
  wireExplorer();
  const ask = (cmd, fallback) => invoke(cmd).catch(() => fallback);
  S.recs = new Map(((await ask("recs", [])) || []).map((x) => [x.id, x]));
  // The first launch explains itself before scanning if macOS is going to hide things; after that, straight in.
  if (!pref(STARTED_KEY) && !(await ask("full_disk_access", true))) {
    S.intro = true;
    S.disk = await ask("disk", S.disk);
    S.usedAtScan = S.disk.total - S.disk.free;
    renderAll();
  } else {
    scan();
  }
}

if (!TAURI) {
  document.body.innerHTML = '<p class="empty">Open this page inside the Storage app.</p>';
} else {
  start();
}
