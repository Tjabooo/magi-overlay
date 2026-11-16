// Shared overlay widgets. Each takes a container element and keeps itself up to date.
import { S, on, h, $, set, now, dur, fmt, mapName, tierColor, every, liveSince, clock } from "./magi.js";

/* ───────── chat ───────── */
const NAME_COLORS = ["#ff8c1a", "#f5a524", "#e8a36a", "#d98c5f", "#c9a27e", "#ffb366", "#e5484d", "#d8cbbf"];
function readable(hex, login) {
  if (!/^#[0-9a-f]{6}$/i.test(hex || "")) {
    let x = 0;
    for (const c of login || "") x = (x * 31 + c.charCodeAt(0)) >>> 0;
    return NAME_COLORS[x % NAME_COLORS.length];
  }
  let [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  const lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
  if (lum < 110) {
    // Too dark on the ember background: lift towards white.
    const k = (110 - lum) / 160 + 0.25;
    [r, g, b] = [r, g, b].map((c) => Math.round(c + (255 - c) * k));
  }
  return `rgb(${r},${g},${b})`;
}

function msgEl(m) {
  const e = h("div", "msg" + (m.action ? " action" : "") + (m.kind === "announcement" ? " announcement" : ""));
  e.dataset.id = m.id || "";
  e.dataset.uid = m.user_id || "";
  e.style.setProperty("--c", readable(m.color, m.login));
  for (const b of m.badges || []) {
    const i = h("img", "badge");
    i.src = b;
    e.append(i);
  }
  e.append(h("span", "who", m.user));
  const t = h("span", "text");
  for (const p of m.parts || []) {
    if (p.t === "emote") {
      const i = h("img", "emote");
      i.src = p.url;
      i.alt = p.v;
      t.append(i);
    } else t.append(document.createTextNode(p.v));
  }
  e.append(t);
  return e;
}

/** Chat list. fade = seconds before a message fades (0 = never). */
export function chat(el, { fade = 0, max = 30 } = {}) {
  const fadeS = () => (typeof fade === "function" ? fade() : fade) || 0;
  const add = (m, animate = true) => {
    const fade = fadeS();
    if (fade && now() - (m.ts || 0) > fade * 1000) return;
    const e = msgEl(m);
    if (!animate) e.style.animation = "none";
    el.append(e);
    while (el.children.length > max) el.firstChild.remove();
    if (fade) {
      const left = fade * 1000 - (now() - (m.ts || now()));
      setTimeout(() => { e.classList.add("faded"); setTimeout(() => e.remove(), 900); }, Math.max(0, left));
    }
    // Drop whatever no longer fits (newest stays at the bottom).
    while (el.scrollHeight > el.clientHeight + 2 && el.children.length > 1) el.firstChild.remove();
  };
  on("snapshot", () => { el.replaceChildren(); for (const m of S.chat.slice(-max)) add(m, false); });
  on("chat", (m) => add(m));
  on("chat_remove", (r) => {
    for (const e of [...el.children]) {
      if (r.id ? e.dataset.id === r.id : r.user_id ? e.dataset.uid === r.user_id : true) e.remove();
    }
  });
}

/* ───────── camera frame ───────── */
/** Corner brackets + caption bars around the OBS camera rect (layout key). */
export function camFrame(el, key, { top, bottom } = {}) {
  el.className = "camframe";
  el.innerHTML = '<div class="edge"></div><i></i><i></i><i></i><i></i>';
  on("snapshot", () => {
    const r = S.config.layout?.[key];
    if (!r) return;
    Object.assign(el.style, { left: r.x + "px", top: r.y + "px", width: r.w + "px", height: r.h + "px" });
  });
  if (top) { const c = h("div", "cap top"); el.append(c); top(c); }
  if (bottom) { const c = h("div", "cap bottom"); el.append(c); bottom(c); }
}

/** "◆ PILOT · NAME ……… ● LIVE 01:23:45" caption. */
export function pilotCap(c) {
  const pilot = h("span", "pilot");
  const grow = h("span", "grow");
  const live = h("span", "live num");
  c.append(h("span", "hex", "01"), pilot, grow, live);
  on("snapshot", () => set(pilot, `PILOT · ${S.config.overlay.pilot || ""}`));
  every(1000, () => {
    const since = liveSince();
    if (since) { live.className = "live num"; set(live, `● LIVE ${dur(now() - since, true)}`); }
    else { live.className = "muted num"; set(live, `○ STANDBY ${clock()}`); }
  });
}

/** "MELCHIOR·1 GPU 98% 62°C  ·  ♪ title" caption. */
export function sysCap(c) {
  const sys = h("span", "num");
  const np = h("span", "muted");
  np.style.cssText = "margin-left:auto;max-width:250px;overflow:hidden;text-overflow:ellipsis";
  c.append(h("span", "tag", "MELCHIOR·1"), sys, np);
  on("stats", (s) => set(sys, `CPU ${s.cpu ?? "—"}% ${s.cpu_temp ?? "—"}° · GPU ${s.gpu ?? "—"}% ${s.gpu_temp ?? "—"}°`));
  on("media", (m) => set(np, m.status === "Playing" && m.title ? `♪ ${m.title}` : ""));
}

/* ───────── Leetify dossier ───────── */
function stat(k, cls = "") {
  const e = h("div", "stat " + cls);
  const v = h("div", "v num", "—");
  const bar = h("div", "bar");
  const b = h("b");
  bar.append(b);
  e.append(v, h("div", "k", k), bar);
  return { e, v, b };
}
const pct = (x, d = 0) => (x === null || x === undefined ? "—" : `${Number(x).toFixed(d)}`);
function signed(x, d = 2) { return x === null || x === undefined ? "—" : (x >= 0 ? "+" : "") + Number(x).toFixed(d); }

function formEl(form, size) {
  const f = h("div", "form");
  for (const m of (form || []).slice(0, 10).reverse()) {
    const i = h("i", m.outcome === "loss" ? "loss" : m.outcome === "tie" ? "tie" : "win", mapName(m.map).slice(0, 2));
    if (size) Object.assign(i.style, { width: size + "px", height: size + "px", fontSize: Math.round(size / 3) + "px" });
    f.append(i);
  }
  return f;
}

function deltaEl(d) {
  if (d === null || d === undefined) return h("span", "delta flat", "");
  return h("span", "delta " + (d > 0 ? "up" : d < 0 ? "down" : "flat"), `${d > 0 ? "▲" : d < 0 ? "▼" : "■"} ${d > 0 ? "+" : ""}${fmt(d)}`);
}

/** Compact dossier for the gameplay scene (≈ 496 × 190). */
export function dossierCompact(el) {
  el.className = "panel dossier";
  el.innerHTML = `
    <div class="ph"><span class="tag">PILOT DOSSIER</span><span class="role" data-k="name"></span><span class="right">DATA · LEETIFY</span></div>
    <div style="display:flex;align-items:center;gap:16px;padding:14px 20px 0">
      <span class="premier-badge num" data-k="premier">—</span><span data-k="delta"></span>
      <div class="stat" style="margin-left:auto;align-items:flex-end"><div class="v num" data-k="aim" style="color:var(--gold)">—</div><div class="k">AIM</div></div>
    </div>
    <div data-k="grid" style="display:grid;grid-template-columns:repeat(4,1fr);gap:14px;padding:14px 20px 0"></div>
    <div style="display:flex;align-items:center;gap:12px;padding:14px 20px 18px"><span data-k="form"></span><span class="label" data-k="session" style="margin-left:auto"></span></div>`;
  const q = (k) => el.querySelector(`[data-k="${k}"]`);
  const cells = ["WIN", "HS", "STRAFE", "RATING"].map((k) => stat(k));
  for (const c of cells) { c.v.style.fontSize = "24px"; q("grid").append(c.e); }
  on("leetify", (L) => {
    el.classList.toggle("hidden", !L || !L.premier && !L.aim);
    set(q("name"), (L.name || "").toUpperCase());
    const p = q("premier");
    p.style.setProperty("--tier", tierColor(L.premier));
    p.style.fontSize = "30px"; p.style.height = "44px";
    set(p, L.premier ? fmt(L.premier) : "UNRANKED");
    q("delta").replaceChildren(deltaEl(L.session?.premier_delta));
    set(q("aim"), pct(L.aim, 1));
    const vals = [[pct(L.winrate), "%", L.winrate], [pct(L.hs_accuracy), "%", L.hs_accuracy], [pct(L.counter_strafe), "%", L.counter_strafe], [signed(L.leetify), "", 50 + (L.leetify || 0) * 5]];
    vals.forEach(([v, unit, w], i) => { cells[i].v.innerHTML = ""; cells[i].v.append(v, h("small", "", unit)); cells[i].b.style.width = Math.max(0, Math.min(100, w || 0)) + "%"; });
    q("form").replaceChildren(formEl(L.form, 26));
    const s = L.session || {};
    set(q("session"), s.wins + s.losses ? `TODAY ${s.wins}W · ${s.losses}L` : `${fmt(L.matches)} MATCHES`);
  });
}

/** Full dossier for the big scenes (≈ 752 wide). */
export function dossierFull(el) {
  el.className = "panel dossier";
  el.innerHTML = `
    <div class="ph"><span class="hex">03</span><span class="tag">PILOT DOSSIER</span><span class="role" data-k="name"></span><span class="right">DATA · LEETIFY</span></div>
    <div style="display:flex;align-items:center;gap:28px;padding:22px 24px 0">
      <div class="aimring"><svg viewBox="0 0 100 100"><circle cx="50" cy="50" r="44" fill="none" stroke="#3a2c22" stroke-width="7"/>
        <circle data-k="ring" cx="50" cy="50" r="44" fill="none" stroke="url(#g)" stroke-width="7" stroke-linecap="butt" stroke-dasharray="276.5" stroke-dashoffset="276.5" style="transition:stroke-dashoffset 1.4s cubic-bezier(.2,.8,.2,1)"/>
        <defs><linearGradient id="g"><stop offset="0" stop-color="#ff4d1f"/><stop offset="1" stop-color="#f5a524"/></linearGradient></defs></svg>
        <div class="val"><b class="num" data-k="aim">—</b><span>AIM</span></div></div>
      <div style="display:flex;flex-direction:column;gap:12px;min-width:0">
        <div class="label">PREMIER RATING</div>
        <div style="display:flex;align-items:center;gap:16px"><span class="premier-badge num" data-k="premier">—</span><span data-k="delta"></span></div>
        <div class="subline" data-k="record" style="font-size:19px"></div>
      </div>
    </div>
    <div data-k="grid" style="display:grid;grid-template-columns:repeat(3,1fr);gap:22px 26px;padding:26px 24px 0"></div>
    <div class="rule"></div>
    <div style="display:flex;align-items:center;gap:14px;padding:0 24px 22px;white-space:nowrap"><div class="label">LAST 10</div><span data-k="form"></span><span class="label" data-k="session" style="margin-left:auto;color:var(--sub)"></span></div>`;
  const q = (k) => el.querySelector(`[data-k="${k}"]`);
  const names = ["WIN RATE", "HEADSHOT ACC", "COUNTER-STRAFE", "T OPENINGS WON", "POSITIONING", "LEETIFY RATING"];
  const cells = names.map((k) => stat(k, ""));
  cells.forEach((c, i) => { c.v.style.fontSize = "38px"; if (i === 5) c.e.querySelector(".bar").classList.add("gold"); q("grid").append(c.e); });
  on("leetify", (L) => {
    set(q("name"), (L.name || "").toUpperCase());
    const p = q("premier");
    p.style.setProperty("--tier", tierColor(L.premier));
    set(p, L.premier ? fmt(L.premier) : "UNRANKED");
    q("delta").replaceChildren(deltaEl(L.session?.premier_delta));
    set(q("aim"), pct(L.aim, 1));
    q("ring").style.strokeDashoffset = String(276.5 * (1 - Math.min(100, L.aim || 0) / 100));
    const best = L.best_map ? ` · BEST ${mapName(L.best_map.map)}` : "";
    set(q("record"), `${fmt(L.matches)} MATCHES${best}`);
    const vals = [[pct(L.winrate, 1), "%", L.winrate], [pct(L.hs_accuracy, 1), "%", L.hs_accuracy], [pct(L.counter_strafe, 1), "%", L.counter_strafe],
      [pct(L.t_opening, 1), "%", L.t_opening], [pct(L.positioning, 1), "", L.positioning], [signed(L.leetify), "", 50 + (L.leetify || 0) * 5]];
    vals.forEach(([v, unit, w], i) => { cells[i].v.innerHTML = ""; cells[i].v.append(v, h("small", "", unit)); cells[i].b.style.width = Math.max(0, Math.min(100, w || 0)) + "%"; });
    q("form").replaceChildren(formEl(L.form, 31));
    const s = L.session || {};
    set(q("session"), s.wins + s.losses ? `${s.wins}W · ${s.losses}L` : "");
  });
}

/* ───────── live CS2 match ───────── */
export function matchStrip(el) {
  el.className = "panel solid match";
  el.style.setProperty("--c", "12px");
  const map = h("span", "map");
  const score = h("span", "score num");
  const kad = h("span", "kad num");
  const extra = h("span", "kad num");
  extra.style.marginLeft = "auto";
  el.append(map, score, kad, extra);
  let lastRoundKills = 0, lastRound = -1;
  const render = () => {
    const c = S.state.cs2, L = S.state.leetify;
    if (c?.active) {
      el.classList.remove("hidden");
      set(map, mapName(c.map));
      const mine = c.team === "T" ? [c.score_t, c.score_ct] : [c.score_ct, c.score_t];
      const [mc, oc] = c.team === "T" ? ["t", "ct"] : ["ct", "t"];
      score.innerHTML = "";
      score.append(h("span", mc, mine[0] ?? 0), " : ", h("span", oc, mine[1] ?? 0));
      kad.innerHTML = "";
      for (const [k, v] of [["K", c.kills], ["A", c.assists], ["D", c.deaths]]) { const s = h("span", "", k); s.append(h("b", "", v ?? 0)); kad.append(s); }
      extra.innerHTML = "";
      for (const [k, v] of [["HS", c.hs_pct != null ? c.hs_pct + "%" : "—"], ["ADR", c.adr ?? "—"]]) { const s = h("span", "", k); s.append(h("b", "", v)); extra.append(s); }
      // Flash on every kill this round.
      if (c.round !== lastRound) { lastRound = c.round; lastRoundKills = 0; }
      if ((c.round_kills || 0) > lastRoundKills) { el.classList.remove("flash"); void el.offsetWidth; el.classList.add("flash"); }
      lastRoundKills = c.round_kills || 0;
    } else if (L?.form?.length) {
      el.classList.remove("hidden");
      const m = L.form[0];
      set(map, "LAST OP");
      score.innerHTML = "";
      score.append(h("span", m.outcome === "win" ? "t" : "", `${mapName(m.map)} ${m.score?.[0] ?? "?"}–${m.score?.[1] ?? "?"}`));
      kad.innerHTML = "";
      kad.append(h("span", "", m.outcome === "win" ? "VICTORY" : m.outcome === "loss" ? "DEFEAT" : "DRAW"));
      extra.innerHTML = "";
      if (m.rating != null) { const s = h("span", "", "RATING"); s.append(h("b", "", signed(m.rating * 100))); extra.append(s); }
    } else el.classList.add("hidden");
  };
  on("cs2", render);
  on("leetify", render);
}

/* ───────── MAGI unit cards (full scenes) ───────── */
function row(k, bar = true) {
  const r = h("div", "row");
  const b = h("div", "bar");
  const fill = h("b");
  b.append(fill);
  const v = h("div", "v");
  r.append(h("div", "k", k), bar ? b : h("div"), v);
  return { r, v, fill };
}
function unitShell(n, name, role) {
  const u = h("div", "panel unit");
  const ph = h("div", "ph");
  ph.append(h("span", "hex", "0" + n), h("span", "tag", name), h("span", "role", role));
  const rows = h("div", "rows");
  u.append(ph, rows);
  return { u, rows, ph };
}
export function units(el) {
  el.classList.add("units");
  // MELCHIOR: performance
  const m = unitShell(1, "MELCHIOR", "PERFORMANCE");
  const cpu = row("CPU"), gpu = row("GPU"), vram = row("VRAM"), ram = row("MEMORY");
  m.rows.append(cpu.r, gpu.r, vram.r, ram.r);
  on("stats", (s) => {
    set(cpu.v, `${s.cpu ?? "—"}% · ${s.cpu_temp ?? "—"}°`); cpu.fill.style.width = (s.cpu || 0) + "%";
    set(gpu.v, `${s.gpu ?? "—"}% · ${s.gpu_temp ?? "—"}°`); gpu.fill.style.width = (s.gpu || 0) + "%";
    set(vram.v, `${s.vram_used ?? "—"}/${s.vram_total ?? "—"} GB`); vram.fill.style.width = (100 * (s.vram_used || 0) / (s.vram_total || 1)) + "%";
    set(ram.v, `${s.ram_used ?? "—"}/${s.ram_total ?? "—"} GB`); ram.fill.style.width = (100 * (s.ram_used || 0) / (s.ram_total || 1)) + "%";
  });
  // BALTHASAR: comms / now playing
  const b = unitShell(2, "BALTHASAR", "COMMS");
  const np = h("div", "np"), t = h("div", "t", "—"), a = h("div", "a", "no audio feed");
  np.append(h("div", "label", "NOW PLAYING"), t, a);
  const msgs = row("MESSAGES", false), chatters = row("CHATTERS", false);
  b.rows.append(np, msgs.r, chatters.r);
  on("media", (x) => {
    const playing = x.status === "Playing" && x.title;
    set(t, playing ? x.title : x.title ? "PAUSED" : "—");
    set(a, playing ? x.artist || x.player || "" : x.title ? `${x.title}${x.artist ? " — " + x.artist : ""}` : "no audio feed");
  });
  on("session", (s) => { set(msgs.v, fmt(s.messages || 0)); set(chatters.v, fmt((s.chatters || []).length)); });
  // CASPER: channel
  const c = unitShell(3, "CASPER", "CHANNEL");
  const fol = row("FOLLOWERS", false), last = row("LATEST", false), sub = row("LAST SUB", false), viewers = row("VIEWERS", false);
  c.rows.append(viewers.r, fol.r, last.r, sub.r);
  on("stream", (s) => {
    set(fol.v, fmt(s.followers)); set(last.v, s.latest_follower || "—"); set(viewers.v, s.live ? fmt(s.viewers) : "OFFLINE");
  });
  on("session", (s) => set(sub.v, s.latest_sub || "—"));
  el.append(m.u, b.u, c.u);
}

/* ───────── top bar + ticker ───────── */
export function topbar(el, label) {
  el.className = "topbar";
  const brand = h("div", "brand");
  brand.append(h("span", "hex", "M"), document.createTextNode("MAGI"), h("span", "role", label || "BROADCAST UNIT"));
  const mid = h("div", "mid");
  const meta = h("div", "meta");
  const viewers = h("span"), up = h("span"), clk = h("span");
  meta.append(viewers, up, clk);
  el.append(brand, mid, meta);
  on("stream", (s) => {
    set(mid, [s.game, s.title].filter(Boolean).join("  ·  "));
    viewers.innerHTML = ""; if (s.live) { viewers.append("VIEWERS "); viewers.append(h("b", "num", fmt(s.viewers))); }
  });
  every(1000, () => {
    const since = liveSince();
    up.innerHTML = ""; up.append(h("span", "dot" + (since ? " live" : "")), " ", since ? h("b", "num", dur(now() - since, true)) : "STANDBY");
    clk.innerHTML = ""; clk.append(h("b", "num", clock()));
  });
}

export function ticker(el) {
  el.className = "ticker";
  const line = h("div", "line");
  const np = h("div", "np-inline");
  el.append(h("span", "tag", "MAGI ▸"), line, np);
  let i = 0;
  const lines = () => {
    const out = [...(S.config.overlay?.ticker || [])];
    const st = S.state.stream || {}, se = S.session || {};
    if (st.latest_follower) out.push(`LATEST PILOT REGISTERED · ${st.latest_follower}`);
    if (se.latest_sub) out.push(`LATEST SYNC CONTRACT · ${se.latest_sub}`);
    const L = S.state.leetify;
    if (L?.premier) out.push(`PREMIER ${fmt(L.premier)} · AIM ${Number(L.aim || 0).toFixed(1)} · WIN RATE ${Number(L.winrate || 0).toFixed(1)}%`);
    if (S.config.channel) out.push(`twitch.tv/${S.config.channel}`);
    return out.length ? out : ["ALL UNITS NOMINAL"];
  };
  const swap = () => {
    line.classList.add("out");
    setTimeout(() => { const l = lines(); set(line, l[i++ % l.length]); line.classList.remove("out"); }, 400);
  };
  on("snapshot", swap);
  setInterval(swap, 9000);
  on("media", (m) => set(np, m.status === "Playing" && m.title ? `♪ ${m.title}${m.artist ? " — " + m.artist : ""}` : ""));
}
