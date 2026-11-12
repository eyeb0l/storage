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
};

const CATS = [
  { id: "worktrees", name: "Agent worktrees", color: "#ff4d9d", desc: "Checkouts coding agents make for each task, each with its own dependencies and build output." },
  { id: "agents", name: "Agent data", color: "#a472ff", desc: "Agent apps, plus the transcripts, logs, temp files and CLI versions Claude Code, Codex, T3 Code and others keep." },
  { id: "code", name: "Projects", color: "#4d82ff", desc: "Your source code, not counting dependencies and build output." },
  { id: "deps", name: "Dependencies", color: "#22d0f0", desc: "node_modules and Python virtual environments. Reinstalled from your lockfiles." },
  { id: "build", name: "Build output", color: "#3ddb7a", desc: "target, .next, DerivedData and other build folders. Recreated on the next build." },
  { id: "caches", name: "Caches & temp", color: "#ff8a2a", desc: "Package downloads, app caches and temporary files. Fetched or rebuilt when they're needed." },
  { id: "toolchains", name: "Dev tools & SDKs", color: "#f5c623", desc: "Xcode, editors, simulator runtimes, Homebrew, language versions and Android images." },
  { id: "models", name: "AI models", color: "#a6e33b", desc: "Local model weights from Ollama, LM Studio and Hugging Face." },
  { id: "containers", name: "Containers & VMs", color: "#2ec4b6", desc: "Docker, OrbStack and virtual machine disk images." },
  { id: "trash", name: "Trash", color: "#f2484b", desc: "Files already deleted. Empty the Trash to get the space back." },
  { id: "other", name: "Everything else", color: "#b4b4bc", desc: "Documents, media and app data in your home folder." },
  { id: "system", name: "System & apps", color: "#6e6e78", desc: "macOS, your other apps, and anything Storage can't read." },
];
const CAT = Object.fromEntries(CATS.map((c) => [c.id, c]));

function glyph(id, size = 15) {
  const c = CAT[id];
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
  note: "",
  sugIdx: 0,
};

const now = () => Date.now() / 1000;
const recOf = (e) => S.recs.get(e.rec);
const methodOf = (e) => (e.rec === "trash" ? "empty" : e.cmd ? "command" : recOf(e).tier === "safe" ? "delete" : "trash");
const wtOf = (e) => (e.parent ? S.entries.get(e.parent) : e.rec === "worktrees" ? e : null);
function inUse(e) {
  const w = wtOf(e);
  return !!(w && w.worktree && now() - w.worktree.last_active < ACTIVE_SECONDS);
}
function wtRemovable(e) {
  const w = e.worktree;
  return !!w && !inUse(e) && (w.orphaned || (w.dirty === 0 && w.unique === 0 && (w.merged || w.gone)));
}
const usedNow = () => (S.phase === "scanning" ? S.scanned : Math.max(0, S.usedAtScan - S.freed));
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

class DotBar {
  constructor(card, slot, canvas) {
    this.card = card;
    this.slot = slot;
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.w = Object.fromEntries(CATS.map((c) => [c.id, 0]));
    this.parts = [];
    this.shownUsed = 0;
    this.last = performance.now();
    new ResizeObserver(() => this.resize()).observe(card);
    this.resize();
    requestAnimationFrame((t) => this.frame(t));
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
    const dt = Math.min(64, t - this.last);
    this.last = t;
    const k = 1 - Math.exp(-dt / 240);
    for (const [id, target] of this.targets()) this.w[id] += (target - this.w[id]) * k;

    // header number
    const used = usedNow();
    this.shownUsed += (used - this.shownUsed) * (1 - Math.exp(-dt / 260));
    if (Math.abs(used - this.shownUsed) < 5e6) this.shownUsed = used;
    const text = gb(this.shownUsed);
    if (text !== this.usedText) $("#usedNum").textContent = this.usedText = text;

    this.draw(t, dt);
    requestAnimationFrame((tt) => this.frame(tt));
  }

  draw(t, dt) {
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

    // dots
    let si = 0;
    for (let c = 0; c < this.cols; c++) {
      const dx = this.ox + c * PITCH;
      const mid = dx + DOT / 2;
      while (si < spans.length && mid >= spans[si].x1) si++;
      const span = si < spans.length && mid >= spans[si].x0 ? spans[si] : null;
      const edge = scanning && span && mid > usedEnd - PITCH * 2.2;
      ctx.fillStyle = edge ? "#e8f1ff" : span ? CAT[span.id].color : "#ffffff";
      const dim = hover && span && span.id !== hover ? 0.32 : 1;
      for (let r = 0; r < this.rows; r++) {
        const i = c * this.rows + r;
        let a;
        if (!span) a = 0.045;
        else if (edge) a = 0.55 + 0.45 * Math.random();
        else {
          const depth = (r + 1) / this.rows;
          const lit = this.seed[i] < 0.07 + 0.6 * depth ** 2.4;
          const ph = this.phase[i];
          if (lit) a = 0.55 + 0.45 * Math.sin(t * 0.0013 * (0.4 + ph) + ph * 40) ** 2;
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

    if (scanning && spans.length) {
      const g = ctx.createLinearGradient(usedEnd - 26, 0, usedEnd + 4, 0);
      g.addColorStop(0, "rgba(90,150,255,0)");
      g.addColorStop(1, "rgba(120,170,255,0.35)");
      ctx.fillStyle = g;
      ctx.fillRect(usedEnd - 26, bar.y, 30, bar.h);
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
      const a = Math.pow(p.life / p.max, 0.9);
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
    html = `<span>Calculating</span><span class="path">${esc(S.scanPath.startsWith("/") || !S.scanPath ? S.scanPath : "~/" + S.scanPath)}</span>`;
  } else if (S.phase === "cleaning" && S.current) {
    html = `<span>${esc(S.current.verb)}</span><span class="path">${esc(S.current.path)}</span>`;
  } else if (S.phase === "error") {
    html = `<span class="err">${esc(S.error)}</span>`;
  } else if (S.note) {
    html = S.note;
  } else if (S.report) {
    html = `<span class="muted">${fmt(S.disk.total - usedNow())} available</span><span class="dim">· ${plural(S.report.files, "file")} scanned in ${Math.max(1, Math.round(S.report.seconds))} s</span>`;
  } else {
    html = "";
  }
  if (el.innerHTML !== html) el.innerHTML = html;
}

function renderHeader() {
  $("#usedOf").textContent = S.disk.total ? `of ${diskSize(S.disk.total)} used` : "";
  const busy = S.phase === "scanning" || S.phase === "cleaning";
  $("#rescan").disabled = busy;
  $("#rescan").classList.toggle("spinning", S.phase === "scanning");
}

function renderLegend() {
  const el = $("#legend");
  if (!el.children.length) {
    el.innerHTML = CATS.map((c) => `<button type="button" class="lg" data-cat="${c.id}">${glyph(c.id)}<span class="n">${esc(c.name)}</span><span class="s num"></span></button>`).join("");
  }
  for (const b of el.children) {
    const n = bytesOf(b.dataset.cat);
    b.querySelector(".s").textContent = gb(n);
    b.classList.toggle("zero", n < 5e7);
    b.classList.toggle("on", S.hover === b.dataset.cat);
  }
  $("#hero").classList.toggle("hovering", !!S.hover);
}

const SUGGEST = {
  caches: (b) => [`Caches are using <b>${fmt(b)}</b>. Tools rebuild them when needed.`, "Clear caches"],
  build: (b, n) => [`Build output in ${plural(n, "folder")} is using <b>${fmt(b)}</b>. Rebuilt on the next build.`, "Clear build output"],
  deps: (b, n) => [`node_modules in ${plural(n, "project")} idle for a month use <b>${fmt(b)}</b>.`, "Clear node_modules"],
  agents: (b) => [`Agent logs, scratch files and old CLI versions use <b>${fmt(b)}</b>.`, "Clear agent leftovers"],
  toolchains: (b) => [`Xcode device support files are using <b>${fmt(b)}</b>.`, "Clear device support"],
  worktrees: (b, n) => [`${plural(n, "finished worktree")} with nothing uncommitted ${n === 1 ? "uses" : "use"} <b>${fmt(b)}</b>.`, "Move to Trash"],
  trash: (b) => [`The Trash is holding <b>${fmt(b)}</b>.`, "Empty Trash"],
};

function suggestions() {
  if (S.phase !== "ready" && S.phase !== "cleaning") return [];
  const groups = new Map();
  const add = (cat, e) => (groups.get(cat) || groups.set(cat, []).get(cat)).push(e);
  for (const e of S.entries.values()) {
    const r = recOf(e);
    if (e.rec === "worktrees") {
      if (wtRemovable(e)) add("worktrees", e);
    } else if (e.rec === "trash") {
      add("trash", e);
    } else if (r.tier === "safe" && !inUse(e) && !(e.idle_days != null && e.idle_days < 3)) {
      if (e.rec === "node_modules" && !(e.idle_days >= 30)) continue;
      add(r.cat, e);
    }
  }
  const out = [];
  for (const [cat, es] of groups) {
    const bytes = sum(es);
    const min = cat === "trash" ? 500e6 : 150e6;
    if (bytes < min || !SUGGEST[cat]) continue;
    const [text, action] = SUGGEST[cat](bytes, es.length);
    out.push({ cat, keys: es.map((e) => e.key), bytes, text, action });
  }
  out.sort((a, b) => (a.cat === "trash") - (b.cat === "trash") || b.bytes - a.bytes);
  return out;
}

function renderSuggest() {
  const el = $("#suggest");
  if (S.phase === "scanning" || S.phase === "idle") {
    el.innerHTML = `${SPINNER}<span class="text">Looking for things you can clear</span>`;
    return;
  }
  if (S.phase === "error") {
    el.innerHTML = `<span class="text">The scan didn't finish.</span><button type="button" class="btn" data-act="rescan">Try again</button>`;
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
    ? `<span class="pager"><button type="button" data-act="prev" aria-label="Previous suggestion">‹</button>${S.sugIdx + 1}/${list.length}<button type="button" data-act="next" aria-label="Next suggestion">›</button></span>`
    : "";
  el.innerHTML = `${glyph(s.cat)}<span class="text">${s.text}</span>${pager}<button type="button" class="btn" data-act="suggest" ${S.phase === "cleaning" ? "disabled" : ""}>${esc(s.action)}</button>`;
  el.dataset.keys = JSON.stringify(s.keys);
}

/* ---------- panels ---------- */

function entryRows(entries, max = 150) {
  const sorted = entries.slice().sort((a, b) => b.size - a.size);
  const more = sorted.slice(max);
  return `<ul class="paths">${sorted.slice(0, max).map((e) => `
      <li><label><input type="checkbox" data-key="${esc(e.key)}">
        <span><span class="pn">${esc(entryName(e))}</span><span class="pm">${esc(entryMeta(e))}</span></span>
        <span class="ps num">${fmt(e.size)}</span>
        <button type="button" class="reveal" data-reveal="${esc(e.key)}" title="Show in Finder" aria-label="Show in Finder">${FINDER}</button></label></li>`).join("")}
    ${more.length ? `<li class="more">and ${more.length} more, ${fmt(sum(more))}. Selecting the group includes them.</li>` : ""}</ul>`;
}

function entryName(e) {
  if (e.rec === "node_modules" || e.rec === "venv") return disp(e.label || e.path);
  return e.label || disp(e.path);
}

function entryMeta(e) {
  const bits = [];
  if (e.parent) bits.push(inUse(e) ? "in a worktree that's in use" : "in a worktree");
  if (e.idle_days != null) bits.push(idleText(e.idle_days));
  if (e.mtime) bits.push("modified " + monthYear(e.mtime));
  bits.push(e.cmd ? e.cmd.join(" ") : disp(e.path));
  return bits.filter(Boolean).join("  ·  ");
}

function wtChips(e) {
  const w = e.worktree || {};
  const chips = [];
  if (inUse(e)) chips.push(["blue", "in use"]);
  if (w.orphaned) chips.push(["", "orphaned"]);
  else {
    chips.push(w.dirty ? ["amber", `${w.dirty} uncommitted`] : ["green", "clean"]);
    if (w.unique) chips.push(["amber", `${w.unique} commits only here`]);
    chips.push(w.merged ? ["green", "merged"] : w.gone ? ["green", "remote branch deleted"] : w.pushed ? ["", "pushed"] : ["", "not merged"]);
  }
  return `<span class="chips">${chips.map(([c, t]) => `<span class="chip ${c}">${esc(t)}</span>`).join("")}</span>`;
}

function renderWorktrees() {
  const wts = [...S.entries.values()].filter((e) => e.rec === "worktrees").sort((a, b) => b.size - a.size);
  $("#wtPanel").hidden = !wts.length;
  if (!wts.length) return;
  const total = sum(wts);
  const byAgent = new Map();
  for (const e of wts) {
    const a = e.worktree?.agent || "Other git worktrees";
    byAgent.set(a, (byAgent.get(a) || 0) + e.size);
  }
  const summary = [...byAgent].sort((a, b) => b[1] - a[1]).map(([a, b]) => `${a} ${fmt(b)}`).join(" · ");
  $("#wtList").innerHTML = `<div class="group-head"><span class="muted">${esc(summary)}</span><span class="num">${fmt(total)}</span></div>`
    + wts.map((e) => {
      const w = e.worktree || {};
      const repo = (w.repo || "").split("/").pop() || e.path.split("/").slice(-2, -1)[0] || "worktree";
      const kids = [...S.entries.values()].filter((k) => k.parent === e.key);
      const meta = [w.agent || "git", disp(e.path), w.last_active ? "active " + ago(now() - w.last_active) : ""].filter(Boolean).join("  ·  ");
      const open = S.open.has(e.key);
      const notes = [];
      if (w.orphaned) notes.push("Its repository no longer tracks it, so there's nothing for git to lose. Safe to remove.");
      if (w.dirty) notes.push(`<span class="warn">${plural(w.dirty, "file")} with uncommitted changes. They go to the Trash with the folder, so you can still get them back.</span>`);
      if (w.unique) notes.push(`<span class="warn">${plural(w.unique, "commit")} on a detached HEAD that no branch contains.</span>`);
      if (inUse(e)) notes.push("Touched in the last two hours, so an agent may still be working in it.");
      if (!notes.length && w.merged) notes.push("Everything here is already part of the default branch.");
      if (!notes.length && w.gone) notes.push("Its branch was deleted on the remote, which usually means the pull request was merged.");
      if (w.branch && !w.orphaned) notes.push(`The branch <code>${esc(w.branch)}</code> stays in ${esc(w.repo || "the repo")} after the folder goes.`);
      return `<div class="item${open ? " open" : ""}" data-open="${esc(e.key)}">
        <div class="row">
          <input type="checkbox" data-key="${esc(e.key)}" aria-label="Select worktree ${esc(e.path)}">
          ${glyph("worktrees")}
          <div><div class="ti">${esc(repo)}<span class="br">${esc(w.branch || "detached HEAD")}</span></div><div class="hi">${esc(meta)}</div></div>
          ${wtChips(e)}
          <div class="sz num">${fmt(e.size)}</div>
          <span class="chev">${CHEV}</span>
        </div>
        <div class="detail"${open ? "" : " hidden"}>
          ${notes.map((n) => `<p>${n}</p>`).join("")}
          ${kids.length ? `<p class="muted">Or clear just the rebuildable parts:</p>${entryRows(kids)}` : ""}
          <p><button type="button" class="linkbtn" data-reveal="${esc(e.key)}">Show in Finder</button></p>
        </div>
      </div>`;
    }).join("");
}

function renderRecs() {
  const recs = new Map();
  for (const e of S.entries.values()) {
    if (e.rec === "worktrees") continue;
    const r = recs.get(e.rec) || recs.set(e.rec, { rec: recOf(e), entries: [] }).get(e.rec);
    r.entries.push(e);
  }
  const shown = [...recs.values()].map((r) => ({ ...r, total: sum(r.entries) })).filter((r) => r.total >= 10e6).sort((a, b) => b.total - a.total);
  $("#recPanel").hidden = !shown.length && !S.report?.unreadable.length;
  const tiers = [
    { id: "safe", label: "Safe to delete", sub: "deleted right away" },
    { id: "review", label: "Review first", sub: "moved to the Trash" },
  ];
  $("#recList").innerHTML = tiers.map((t) => {
    const rs = shown.filter((r) => r.rec.tier === t.id);
    if (!rs.length) return "";
    return `<div class="group-head"><span><span class="pill ${t.id}">${t.label}</span><small>${t.sub}</small></span><span class="num">${fmt(rs.reduce((a, r) => a + r.total, 0))}</span></div>`
      + rs.map((r) => {
        const open = S.open.has("rec:" + r.rec.id);
        const how = r.rec.id === "trash" ? "Emptying the Trash can't be undone." : r.entries.some((e) => e.cmd) ? `Runs ${esc(r.entries.find((e) => e.cmd).cmd[0])} for each item.` : "";
        return `<div class="item${open ? " open" : ""}" data-open="rec:${r.rec.id}" data-cat="${r.rec.cat}">
          <div class="row">
            <input type="checkbox" data-rec="${r.rec.id}" aria-label="Select ${esc(r.rec.title)}">
            ${glyph(r.rec.cat)}
            <div><div class="ti">${esc(r.rec.title)}</div><div class="hi">${esc(r.rec.hint)}</div></div>
            <span></span>
            <div class="sz num">${r.rec.approx ? "<small>up to </small>" : ""}${fmt(r.total)}</div>
            <span class="chev">${CHEV}</span>
          </div>
          <div class="detail"${open ? "" : " hidden"}>
            <p>${esc(r.rec.desc)}${how ? " " + how : ""}</p>
            ${entryRows(r.entries)}
          </div>
        </div>`;
      }).join("");
  }).join("") || (S.report ? '<p class="empty">Nothing worth clearing was found.</p>' : "");

  const un = S.report?.unreadable || [];
  $("#unreadable").innerHTML = un.length
    ? `<p class="note">${plural(un.length, "folder")} couldn't be read, so ${un.length === 1 ? "it counts" : "they count"} as 0 bytes. To include them, give Storage Full Disk Access in <button type="button" class="linkbtn" data-act="privacy">Privacy &amp; Security</button> and scan again.</p>`
    : "";
}

function syncChecks() {
  for (const box of document.querySelectorAll("input[data-rec]")) {
    const es = [...S.entries.values()].filter((e) => e.rec === box.dataset.rec);
    const n = es.filter((e) => S.sel.has(e.key)).length;
    box.checked = n > 0 && n === es.length;
    box.indeterminate = n > 0 && n < es.length;
  }
  for (const box of document.querySelectorAll("input[data-key]")) {
    const e = S.entries.get(box.dataset.key);
    box.checked = S.sel.has(box.dataset.key) || !!(e && e.parent && S.sel.has(e.parent));
    box.disabled = !!(e && e.parent && S.sel.has(e.parent));
  }
  const eff = effective(S.sel);
  $("#selbar").hidden = !eff.length || S.phase !== "ready";
  $("#selSize").textContent = fmt(sum(eff));
  $("#selCount").textContent = "selected, " + plural(eff.length, "item");
}

function renderPanels() {
  renderWorktrees();
  renderRecs();
  syncChecks();
  renderExplorer();
}

function renderLive() {
  renderHeader();
  renderStatus();
  renderLegend();
}

function renderAll() {
  renderLive();
  renderSuggest();
  renderPanels();
}

/* ---------- confirm & clean ---------- */

let pendingKeys = null;

function confirmClear(keys) {
  const es = effective(new Set(keys));
  if (!es.length) return;
  pendingKeys = es.map((e) => e.key);
  const by = { delete: [], trash: [], command: [], empty: [] };
  for (const e of es) by[methodOf(e)].push(e);
  const groupByRec = (list) => {
    const m = new Map();
    for (const e of list) {
      const r = recOf(e);
      const g = m.get(r.id) || m.set(r.id, { title: r.title, size: 0, n: 0 }).get(r.id);
      g.size += e.size;
      g.n++;
    }
    return [...m.values()].sort((a, b) => b.size - a.size).map((g) => `<li><span>${esc(g.title)}${g.n > 1 ? ` <span class="muted">(${g.n})</span>` : ""}</span><span class="num">${fmt(g.size)}</span></li>`).join("");
  };
  const section = (title, list, items) => (list.length ? `<div class="cf-group"><h3>${title}<span class="num">${fmt(sum(list))}</span></h3><ul>${items}</ul></div>` : "");
  const warns = [];
  const dirty = es.filter((e) => e.rec === "worktrees" && e.worktree?.dirty);
  if (dirty.length) warns.push(`${plural(dirty.length, "worktree")} ${dirty.length === 1 ? "has" : "have"} uncommitted changes. They go to the Trash with the folder.`);
  const busy = es.filter(inUse);
  if (busy.length) warns.push(`${plural(busy.length, "item")} ${busy.length === 1 ? "is" : "are"} in a worktree touched in the last two hours. An agent may still be using it.`);
  if (by.empty.length) warns.push("Emptying the Trash can't be undone.");
  $("#confirmSize").textContent = fmt(sum(es));
  $("#confirmBody").innerHTML = warns.map((w) => `<p class="cf-warn">${esc(w)}</p>`).join("")
    + section("Delete permanently", by.delete, groupByRec(by.delete))
    + section("Move to the Trash", by.trash, groupByRec(by.trash))
    + section("Run", by.command, by.command.map((e) => `<li><span class="mono">${esc(e.cmd.join(" "))}</span><span class="num">${e.rec === "docker" ? "up to " : ""}${fmt(e.size)}</span></li>`).join(""))
    + section("Empty the Trash", by.empty, by.empty.map((e) => `<li><span>~/.Trash</span><span class="num">${fmt(e.size)}</span></li>`).join(""));
  $("#confirmOk").textContent = by.delete.length || by.empty.length ? `Clear ${fmt(sum(es))}` : "Move to Trash";
  $("#confirmOk").classList.toggle("danger", !!by.empty.length);
  $("#confirm").returnValue = ""; // Escape keeps the previous value otherwise
  $("#confirm").showModal();
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
  S.phase = "cleaning";
  S.note = "";
  S.hover = null;
  document.body.classList.add("busy");
  renderAll();
  let deleted = 0, trashed = 0, docker = false;
  const errors = [];
  const ch = new TAURI.core.Channel();
  ch.onmessage = (ev) => {
    const e = S.entries.get(ev.key);
    if (!e) return;
    if (ev.kind === "start") {
      S.current = { verb: verbFor(e), path: e.cmd ? e.label || disp(e.path) : disp(e.path) };
      bar.spawn(e.cat, ...edgeOf(e.cat), 10);
    } else if (ev.kind === "done") {
      if (ev.method === "trash") trashed += e.size;
      else if (e.rec === "docker") docker = true;
      else deleted += e.size;
      removeEntry(e, ev.method);
      renderSuggest();
    } else {
      errors.push(ev.error);
    }
    renderLive();
  };
  try {
    await invoke("clean", { keys, onProgress: ch });
  } catch (err) {
    errors.push(String(err));
  }
  S.current = null;
  S.phase = "ready";
  document.body.classList.remove("busy");
  const parts = [];
  if (deleted) parts.push(`Freed ${fmt(deleted)}`);
  if (trashed) parts.push(`${deleted ? "moved" : "Moved"} ${fmt(trashed)} to the Trash`);
  if (docker) parts.push(`${parts.length ? "pruned" : "Pruned"} Docker`);
  const head = parts.length ? `<span class="ok">${CHECK}</span><b>${esc(parts.join(", "))}.</b>` : "";
  const tail = errors.length
    ? `<span class="warn" title="${esc(errors.join("\n"))}">${plural(errors.length, "item")} couldn't be cleared.</span>`
    : `<span class="muted">${fmt(S.disk.total - usedNow())} available</span>`;
  S.note = head + tail;
  S.sugIdx = 0;
  renderAll();
}

function edgeOf(id) {
  const s = bar.spans().find((x) => x.id === id);
  return s ? [Math.max(s.x0, s.x1 - PITCH * 3), s.x1] : [bar.bar.x, bar.bar.x];
}

/* ---------- scan ---------- */

async function scan() {
  Object.assign(S, { phase: "scanning", error: "", note: "", report: null, cats: {}, scanned: 0, files: 0, scanPath: "", system: 0, freed: 0, hover: null });
  S.entries.clear();
  S.sel.clear();
  EX.root = null;
  $("#tm").replaceChildren();
  renderAll();
  try {
    S.disk = await invoke("disk");
  } catch (_) { /* shown once the scan returns */ }
  renderHeader();
  const ch = new TAURI.core.Channel();
  ch.onmessage = (p) => {
    if (S.phase !== "scanning") return;
    S.cats = p.cats;
    S.scanned = p.bytes;
    S.files = p.files;
    S.scanPath = p.path;
    renderLive();
  };
  try {
    const r = await invoke("scan", { onProgress: ch });
    S.report = r;
    S.disk = r.disk;
    S.cats = { ...r.cats };
    S.scanned = r.scanned;
    S.usedAtScan = r.disk.total - r.disk.free;
    S.system = Math.max(0, S.usedAtScan - r.scanned);
    S.recs = new Map(r.recs.map((x) => [x.id, x]));
    S.entries = new Map(r.entries.map((e) => [e.key, e]));
    indexTree(r.tree);
    S.phase = "ready";
  } catch (err) {
    S.phase = "error";
    S.error = String(err);
  }
  renderAll();
}

/* ---------- explorer (drill-down) ---------- */

/* A squarified treemap of the scan tree: two levels at a time, click a folder to go in, Escape to go
   back. Tiles that are cleanup entries get a checkbox that shares the selection with the lists. */

const EX = { root: null, view: null, byPath: new Map(), tiles: [] };

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
  const box = cov && cov.exact && S.phase === "ready" && r.w >= 70 && r.h >= 40
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
  el.style.cssText = `left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;--c:${(CAT[node.t] || CAT.other).color}`;
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
  if (level === 0 && canOpen(node)) {
    el.tabIndex = 0;
    el.setAttribute("role", "button");
    el.setAttribute("aria-label", `Open ${nodeLabel(node)}, ${fmt(node.s)}`);
  }
  parent.appendChild(el);
}

function renderExplorer() {
  if ($("#exploreView").hidden || !EX.root) return;
  const tm = $("#tm");
  EX.tiles = [];
  tm.replaceChildren();
  const kids = (EX.view.c || []).filter((k) => k.s > 0);
  if (!kids.length) {
    tm.innerHTML = '<div class="tm-empty">Nothing here is larger than 8 MB.</div>';
  } else {
    const byPath = entriesByPath();
    const rects = squarify(kids.map((k) => k.s), 0, 0, tm.clientWidth, tm.clientHeight);
    kids.forEach((k, i) => drawTile(tm, k, inset(rects[i], 3), 0, byPath));
  }
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
  const cov = coveringEntry(node.path, entriesByPath());
  let rec = "";
  if (cov) {
    const r = recOf(cov.entry);
    const state = S.sel.has(cov.entry.key) ? "selected" : r.tier === "safe" ? "safe to delete" : "review first";
    rec = `${esc(r.title)}: ${state}`;
  }
  const where = node.path != null ? disp(node.path) : node.o ? `${node.o} items under 8 MB` : node.x ? "measured by macOS, not a folder you can open" : "";
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
  for (const b of document.querySelectorAll(".tabs button")) b.setAttribute("aria-selected", b.dataset.tab === tab);
  $("#cleanView").hidden = tab !== "clean";
  $("#exploreView").hidden = tab !== "explore";
  renderExplorer();
}

function wireExplorer() {
  $(".tabs").addEventListener("click", (ev) => {
    const b = ev.target.closest("button[data-tab]");
    if (b) setTab(b.dataset.tab);
  });
  $("#crumbs").addEventListener("click", (ev) => {
    const b = ev.target.closest("button[data-depth]");
    if (!b) return;
    const chain = [];
    for (let n = EX.view; n; n = n.parent) chain.unshift(n);
    openNode(chain[+b.dataset.depth]);
  });
  $("#revealView").addEventListener("click", () => invoke("reveal_path", { path: EX.view.path }).catch(() => {}));
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
      openNode(EX.tiles[ev.target.dataset.i]);
      tm.querySelector(".tile[tabindex]")?.focus();
    }
  });
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
}

function toggle(keys, on) {
  for (const k of keys) on ? S.sel.add(k) : S.sel.delete(k);
  syncChecks();
  renderExplorer();
}

function wire() {
  const hero = $("#hero");
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
    renderSuggest();
  });

  $("#rescan").addEventListener("click", scan);

  document.addEventListener("change", (ev) => {
    const t = ev.target;
    if (t.dataset.rec) {
      const es = [...S.entries.values()].filter((e) => e.rec === t.dataset.rec);
      toggle(es.map((e) => e.key), !es.every((e) => S.sel.has(e.key)));
    } else if (t.dataset.key) {
      toggle([t.dataset.key], t.checked);
    }
  });

  document.addEventListener("click", (ev) => {
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
    const row = ev.target.closest(".row");
    if (row && !ev.target.matches("input")) toggleOpen(row.parentElement);
  });

  $("#selectMerged").addEventListener("click", () => {
    const keys = [...S.entries.values()].filter((e) => e.rec === "worktrees" && wtRemovable(e)).map((e) => e.key);
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
    if (ev.key === "r" && (ev.metaKey || ev.ctrlKey) && S.phase !== "scanning" && S.phase !== "cleaning") {
      ev.preventDefault();
      scan();
    }
  });
}

function toggleOpen(item) {
  const id = item.dataset.open;
  const detail = item.querySelector(":scope > .detail");
  if (!id || !detail) return;
  const opening = detail.hidden;
  detail.hidden = !opening;
  item.classList.toggle("open", opening);
  opening ? S.open.add(id) : S.open.delete(id);
}

/* ---------- init ---------- */

if (!TAURI) {
  document.body.innerHTML = '<p class="empty">Open this page inside the Storage app.</p>';
} else {
  renderLegend();
  bar = new DotBar($("#hero"), $("#barSlot"), $("#fx"));
  wire();
  wireExplorer();
  scan();
}
