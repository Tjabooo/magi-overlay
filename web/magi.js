// magi-overlay core: live state over Server-Sent Events + tiny DOM helpers.
export const S = { config: { overlay: {}, layout: {} }, state: {}, chat: [], session: {}, offset: 0 };
const subs = {};

export function on(type, fn) {
  (subs[type] ||= []).push(fn);
  // Late subscribers get the current value straight away.
  if (S.state[type]) fn(S.state[type]);
}
function emit(type, m) {
  for (const fn of subs[type] || []) {
    try { fn(m); } catch (e) { console.error(type, e); }
  }
}

export function connect() {
  // ?snapshot = render one state snapshot without a live connection (previews, screenshots).
  if (new URLSearchParams(location.search).has("snapshot")) {
    fetch("/api/state").then((r) => r.json()).then((m) => handle(m));
    return;
  }
  const es = new EventSource("/events");
  es.onmessage = (e) => handle(JSON.parse(e.data));
}

function handle(m) {
  {
    switch (m.type) {
      case "snapshot":
        Object.assign(S, { config: m.config, state: m.state, chat: m.chat, session: m.session, offset: m.now - Date.now() });
        emit("snapshot", m);
        for (const k in m.state) emit(k, m.state[k]);
        emit("session", m.session);
        return;
      case "chat":
        S.chat.push(m);
        if (S.chat.length > 60) S.chat.shift();
        break;
      case "session":
        S.session = m.session;
        emit("session", m.session);
        return;
      case "alert":
      case "chat_remove":
        break;
      default:
        S.state[m.type] = m;
    }
    emit(m.type, m);
  }
}

export const now = () => Date.now() + S.offset;

export function h(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined && text !== null) e.textContent = String(text);
  return e;
}
export const $ = (sel, root = document) => root.querySelector(sel);
export function set(sel, text, root) {
  const e = typeof sel === "string" ? $(sel, root) : sel;
  if (e && e.textContent !== String(text)) e.textContent = String(text);
}

/** Scale the 2560×1440 stage to whatever size OBS gives the browser source. */
export function fit() {
  const stage = $(".stage");
  const go = () => { stage.style.transform = `scale(${innerWidth / 2560})`; };
  go();
  addEventListener("resize", go);
}

export function pad(n) { return String(n).padStart(2, "0"); }
export function dur(ms, alwaysHours = false) {
  if (!(ms >= 0)) return "--:--";
  const s = Math.floor(ms / 1000), hh = Math.floor(s / 3600), mm = Math.floor((s % 3600) / 60), ss = s % 60;
  return hh || alwaysHours ? `${pad(hh)}:${pad(mm)}:${pad(ss)}` : `${pad(mm)}:${pad(ss)}`;
}
export function clock() {
  const d = new Date(now());
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
export function fmt(n) {
  return n === null || n === undefined || Number.isNaN(n) ? "—" : Number(n).toLocaleString("en-US");
}
export function mapName(m) {
  return (m || "").replace(/^(de|cs|ar|dz)_/, "").replace(/_/g, " ").toUpperCase() || "—";
}
/** CS2 Premier rating tier colours. */
export function tierColor(r) {
  if (!r) return "#b0c3d9";
  if (r >= 30000) return "#ffd700";
  if (r >= 25000) return "#eb4b4b";
  if (r >= 20000) return "#f03cff";
  if (r >= 15000) return "#c166ff";
  if (r >= 10000) return "#6a7dff";
  if (r >= 5000) return "#8cc6ff";
  return "#b0c3d9";
}
export function every(ms, fn) { fn(); return setInterval(fn, ms); }

/** Stream start: OBS output start, else Twitch's started_at. */
export function liveSince() {
  const o = S.state.obs, s = S.state.stream;
  if (o?.streaming && o.stream_started_at) return o.stream_started_at;
  if (s?.live && s.started_at) return Date.parse(s.started_at);
  return null;
}
