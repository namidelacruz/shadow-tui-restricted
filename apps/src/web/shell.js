// Shadow shell (10.0): one page, four roles — ?role=taskbar|start|widgets|quick&screen=NAME.
// Shown by shadow-panel in frameless WebKitGTK windows; talks only to shadow-webd's local API
// (and to shadow-panel through window.ipc, to open / close the flyouts).
"use strict";
const $ = (s, el = document) => el.querySelector(s);
const Q = new URLSearchParams(location.search);
const ROLE = Q.get("role") || "taskbar";
const SCREEN = Q.get("screen") || "";
const root = $("#root");
const WEEK_JP = ["日", "月", "火", "水", "木", "金", "土"];
const WEEK_EN = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
const KANJI = ["", "壱", "弐", "参", "肆", "伍", "陸", "漆", "捌", "玖"];

// ── plumbing ─────────────────────────────────────────────────────────────────────────────────
async function unlock() {
  const m = location.hash.match(/k=([0-9a-f]+)/);
  if (!m) return;
  history.replaceState(null, "", location.pathname + location.search);
  await fetch("/api/unlock", { method: "POST", credentials: "same-origin", headers: { "X-Shadow-Key": m[1], "Content-Type": "application/json" }, body: "{}" }).catch(() => {});
}
async function api(method, path, body) {
  const r = await fetch(path, {
    method, credentials: "same-origin",
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const j = await r.json().catch(() => ({ error: "bad reply" }));
  if (!r.ok) throw new Error(j.error || r.statusText);
  return j;
}
const post = (p, b) => api("POST", p, b).catch(e => console.warn(p, e.message));
// to shadow-panel: open / close flyouts
function panel(msg) {
  try { window.ipc.postMessage(JSON.stringify(msg)); } catch (_) { /* outside the panel */ }
}
function el(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "on") for (const [ev, fn] of Object.entries(v)) e.addEventListener(ev, fn);
    else if (k === "text") e.textContent = v;
    else if (k === "class") e.className = v;
    else e.setAttribute(k, v);
  }
  for (const k of kids) if (k != null) e.append(k);
  return e;
}
const GLYPHS = new Map();
function glyph(name, cls = "glyph") {
  const s = el("span", { class: cls });
  const fill = t => { s.innerHTML = t; };
  if (GLYPHS.has(name)) {
    const v = GLYPHS.get(name);
    if (typeof v === "string") fill(v); else v.then(fill);
  } else {
    const p = fetch(`/glyph/${name}.svg`).then(r => r.text()).then(t => { GLYPHS.set(name, t); return t; });
    GLYPHS.set(name, p);
    p.then(fill);
  }
  return s;
}
const ICON = {
  wifi: '<svg viewBox="0 0 16 16"><path d="M1.5 6a9.5 9.5 0 0 1 13 0M3.8 8.4a6.3 6.3 0 0 1 8.4 0M6.1 10.8a3 3 0 0 1 3.8 0"/><circle cx="8" cy="13" r=".9" fill="currentColor"/></svg>',
  wired: '<svg viewBox="0 0 16 16"><path d="M5 2v4h6V2M8 6v4M3 10h10v4H3z"/></svg>',
  off: '<svg viewBox="0 0 16 16"><path d="M2 2l12 12M1.5 6a9.5 9.5 0 0 1 4-2.3M14.5 6a9.5 9.5 0 0 0-5.3-2.4"/></svg>',
  vol: '<svg viewBox="0 0 16 16"><path d="M2 6h3l4-3v10L5 10H2zM11.5 5.5a3.5 3.5 0 0 1 0 5M13 3.5a6.2 6.2 0 0 1 0 9"/></svg>',
  mute: '<svg viewBox="0 0 16 16"><path d="M2 6h3l4-3v10L5 10H2zM11 6l4 4M15 6l-4 4"/></svg>',
  bat: '<svg viewBox="0 0 16 16"><rect x="1.5" y="4.5" width="12" height="7"/><path d="M14.5 7v2"/></svg>',
  plug: '<svg viewBox="0 0 16 16"><path d="M6 2v3M10 2v3M4 5h8v3a4 4 0 0 1-8 0zM8 12v2.5"/></svg>',
};
const icon = (k, extra = "") => el("span", { class: "ic" + extra }, Object.assign(document.createElement("span"), { innerHTML: ICON[k] }).firstChild);
function pad(n) { return String(n).padStart(2, "0"); }

let shell = null;     // compositor state (windows, screens)
let status = null;    // tray values
let apps = null;      // installed apps
const listeners = [];
const onShell = f => listeners.push(f);
function shellStream() {
  let es;
  const open = () => {
    es = new EventSource("/api/shell/events");
    es.onmessage = ev => { try { shell = JSON.parse(ev.data); listeners.forEach(f => f()); } catch (_) {} };
    es.onerror = () => { es.close(); setTimeout(open, 1500); };
  };
  open();
}
async function loadStatus() { try { status = await api("GET", "/api/status"); } catch (_) {} }
async function loadApps() { try { apps = await api("GET", "/api/apps"); } catch (_) { apps = apps || []; } }
function myScreen() {
  if (!shell) return null;
  return shell.screens.find(s => s.name === SCREEN) || shell.screens.find(s => s.focused) || shell.screens[0] || null;
}
// which installed app a window belongs to
function appFor(w) {
  if (!apps) return null;
  const id = (w.app_id || "").toLowerCase();
  if (!id) return null;
  return apps.find(a => a.app_ids.includes(id)) || apps.find(a => a.id.toLowerCase().replace(/\.desktop$/, "") === id) || null;
}
function launch(a, btn) {
  const r = btn ? btn.getBoundingClientRect() : null;
  const s = myScreen();
  // the button's rect in the compositor's global coordinates: the window unfolds from it
  let rect;
  if (r && s && ROLE === "taskbar") rect = [s.x + r.left, s.y + s.h - innerHeight + r.top, r.width, r.height];
  post("/api/launch", { id: a.id, rect });
  if (ROLE !== "taskbar") panel({ do: "hide" });
}

// ── taskbar ─────────────────────────────────────────────────────────────────────────────────
function taskbar() {
  document.body.className = "taskbar";
  const left = el("div", { class: "left" }), mid = el("div", { class: "mid" }), right = el("div", { class: "right" });
  root.append(el("div", { class: "tb" }, left, mid, right));
  // left: widgets (as Windows 11 puts them)
  const wbtn = el("button", { class: "tbtn", on: { click: () => panel({ do: "toggle", role: "widgets" }) } }, glyph("clock"));
  left.append(wbtn);
  const clock = el("button", { class: "tbtn clock", on: { click: () => panel({ do: "toggle", role: "widgets" }) } }, el("span", { class: "t" }), el("span", { class: "d" }));
  const tray = el("button", { class: "tbtn tray", on: { click: () => panel({ do: "toggle", role: "quick" }) } });
  const upd = el("button", { class: "tbtn", style: "display:none", on: { click: () => post("/api/quick", { what: "open", app: "update" }) } }, el("span", { class: "badge al", text: "更新 UPDATE" }));
  right.append(upd, tray, el("div", { class: "sep" }), clock);
  const tick = () => {
    const d = new Date();
    $(".t", clock).textContent = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
    $(".d", clock).replaceChildren(`${d.getFullYear()}·${pad(d.getMonth() + 1)}·${pad(d.getDate())} `, el("span", { class: "jp", text: WEEK_JP[d.getDay()] }));
  };
  tick();
  setInterval(tick, 5000);
  const drawTray = () => {
    if (!status) return;
    const kids = [];
    const n = status.network || {};
    kids.push(icon(n.wired ? "wired" : n.wifi ? "wifi" : "off"));
    const snd = status.sound || {};
    kids.push(icon(snd.muted ? "mute" : "vol"));
    const b = status.battery || {};
    if (b.percent != null) kids.push(el("span", { class: "ic" }, Object.assign(document.createElement("span"), { innerHTML: ICON[b.charging ? "plug" : "bat"] }).firstChild, `${b.percent}%`));
    tray.replaceChildren(...kids);
    upd.style.display = status.update && status.update.ready ? "" : "none";
  };
  const refreshStatus = async () => { await loadStatus(); drawTray(); };
  refreshStatus();
  setInterval(refreshStatus, 4000);

  const draw = () => {
    const s = myScreen();
    const kids = [];
    kids.push(el("button", { class: "tbtn start", on: { click: () => panel({ do: "toggle", role: "start" }) } }, glyph("launch")));
    if (s) {
      kids.push(el("button", { class: "tbtn ws", on: {
        click: () => post("/api/shell", { do: "action", action: "workspace-next" }),
        contextmenu: e => { e.preventDefault(); post("/api/shell", { do: "action", action: "workspace-prev" }); },
        wheel: e => post("/api/shell", { do: "action", action: e.deltaY > 0 ? "workspace-next" : "workspace-prev" }),
      } }, el("span", { class: "jp", text: KANJI[s.workspace] || "" }), el("b", { text: String(s.workspace) })));
    }
    // pinned apps first, then whatever else runs on this screen (any workspace), grouped by app
    const mine = shell ? shell.windows.filter(w => !s || w.screen === s.index) : [];
    const groups = new Map();
    const pins = (apps || []).filter(a => a.pinned != null).sort((a, b) => a.pinned - b.pinned);
    for (const a of pins) groups.set(a.id, { app: a, wins: [] });
    for (const w of mine) {
      const a = appFor(w);
      const key = a ? a.id : "win:" + (w.app_id || w.id);
      if (!groups.has(key)) groups.set(key, { app: a, wins: [], glyph: w.glyph });
      groups.get(key).wins.push(w);
    }
    for (const [, g] of groups) {
      const wins = g.wins;
      const focused = wins.some(w => w.focused && w.visible && !w.minimized);
      const minimized = wins.length > 0 && wins.every(w => w.minimized);
      const cls = "tbtn" + (focused ? " focused" : "") + (minimized ? " minimized" : "");
      const b = el("button", { class: cls }, glyph(g.app ? g.app.glyph : (g.glyph || "generic")));
      if (wins.length) b.append(el("i", { class: "run" }));
      if (wins.length > 1) b.append(el("span", { class: "count", text: String(wins.length) }));
      b.addEventListener("click", () => {
        if (!wins.length) { if (g.app) launch(g.app, b); return; }
        // one window: focus / minimise it; several: the next one gets the keyboard
        const f = wins.findIndex(w => w.focused && w.visible);
        if (wins.length === 1) post("/api/shell", { do: "toggle", id: wins[0].id });
        else post("/api/shell", { do: "focus", id: wins[(f + 1) % wins.length].id });
      });
      b.addEventListener("auxclick", e => { if (e.button === 1 && g.app) launch(g.app, b); });
      b.addEventListener("contextmenu", e => {
        e.preventDefault();
        if (g.app) post("/api/pin", { id: g.app.id, pin: g.app.pinned == null }).then(loadApps).then(draw);
      });
      kids.push(b);
    }
    mid.replaceChildren(...kids);
  };
  onShell(draw);
  loadApps().then(draw);
  setInterval(() => loadApps().then(draw), 60000);
  draw();
}

// ── start ───────────────────────────────────────────────────────────────────────────────────
function start() {
  document.body.className = "fly";
  const input = el("input", { type: "text", placeholder: "Search apps · アプリを検索", spellcheck: "false" });
  const body = el("div", { class: "scroll" });
  const user = el("div", { class: "who" }, glyph("user"), el("span"));
  const pbtn = (g, jp, what, danger) => el("button", { class: "pbtn" + (danger ? " danger" : ""), on: { click: () => { panel({ do: "hide" }); post("/api/power", { what }); } } }, glyph(g), el("span", { class: "jp", text: jp }));
  const foot = el("div", { class: "foot" }, user, pbtn("lock", "施錠", "lock"), pbtn("suspend", "休止", "sleep"), pbtn("logout", "退出", "logout"), pbtn("restart", "再起動", "restart", true), pbtn("power", "電源", "shutdown", true));
  root.append(el("div", { class: "panel" },
    el("div", { class: "search" }, glyph("launch"), input),
    body, foot));
  let all = false, sel = 0, shown = [];
  const appBtn = (a, i) => el("button", { class: "app" + (i === sel ? " sel" : ""), on: {
    click: e => launch(a, e.currentTarget),
    contextmenu: e => { e.preventDefault(); post("/api/pin", { id: a.id, pin: a.pinned == null }).then(loadApps).then(draw); },
  } }, glyph(a.glyph), el("span", { class: "n", text: a.name }), el("span", { class: "j jp", text: a.jp }));
  const rowBtn = (a, i) => el("button", { class: "row" + (i === sel ? " sel" : ""), on: { click: e => launch(a, e.currentTarget) } },
    glyph(a.glyph), el("div", {}, el("div", { text: a.name }), el("div", { class: "d", text: a.detail || a.cats || "" })), el("span", { class: "jp d", text: a.jp }));
  function score(a, q) {
    const n = a.name.toLowerCase(), hay = (a.name + " " + a.detail + " " + a.keywords + " " + a.cats + " " + a.jp).toLowerCase();
    if (n === q) return 100; if (n.startsWith(q)) return 80; if (n.split(/\s+/).some(w => w.startsWith(q))) return 60; if (hay.includes(q)) return 30;
    let i = 0; for (const c of n) if (c === q[i]) i++; return i === q.length ? 10 : -1;
  }
  function draw() {
    const q = input.value.trim().toLowerCase();
    const list = apps || [];
    if (q) {
      shown = list.map(a => [score(a, q), a]).filter(x => x[0] >= 0).sort((a, b) => b[0] - a[0]).map(x => x[1]).slice(0, 40);
      sel = Math.min(sel, Math.max(0, shown.length - 1));
      body.replaceChildren(el("div", { class: "sect" }, el("span", { class: "al", text: "結果 RESULTS" }), el("span", { class: "rule" })),
        shown.length ? el("div", { class: "list" }, ...shown.map(rowBtn)) : el("div", { class: "empty", text: "no app matches · 該当なし" }));
      return;
    }
    if (all) {
      shown = list;
      const kids = [];
      let letter = "";
      list.forEach((a, i) => {
        const l = (a.name[0] || "#").toUpperCase();
        if (l !== letter) { letter = l; kids.push(el("div", { class: "letter", text: l })); }
        kids.push(rowBtn(a, i));
      });
      body.replaceChildren(el("div", { class: "sect" }, el("span", { class: "al", text: "全て ALL APPS" }), el("span", { class: "rule" }), el("button", { text: "‹ pinned", on: { click: () => { all = false; sel = 0; draw(); } } })),
        el("div", { class: "list" }, ...kids));
      return;
    }
    shown = list.filter(a => a.pinned != null).sort((a, b) => a.pinned - b.pinned);
    body.replaceChildren(
      el("div", { class: "sect" }, el("span", { class: "al", text: "固定 PINNED" }), el("span", { class: "rule" }), el("button", { text: "all apps ›", on: { click: () => { all = true; sel = 0; draw(); } } })),
      shown.length ? el("div", { class: "grid" }, ...shown.map(appBtn)) : el("div", { class: "empty", text: "right-click an app to pin it · 右クリックで固定" }));
  }
  input.addEventListener("input", () => { sel = 0; draw(); });
  document.addEventListener("keydown", e => {
    if (e.key === "Escape") { panel({ do: "hide" }); return; }
    const cols = !input.value && !all ? 6 : 1;
    if (e.key === "ArrowDown") { sel = Math.min(shown.length - 1, sel + cols); draw(); e.preventDefault(); }
    else if (e.key === "ArrowUp") { sel = Math.max(0, sel - cols); draw(); e.preventDefault(); }
    else if (e.key === "ArrowRight" && cols > 1) { sel = Math.min(shown.length - 1, sel + 1); draw(); e.preventDefault(); }
    else if (e.key === "ArrowLeft" && cols > 1) { sel = Math.max(0, sel - 1); draw(); e.preventDefault(); }
    else if (e.key === "Enter" && shown[sel]) launch(shown[sel], null);
    else if (e.key.length === 1 && document.activeElement !== input) input.focus();
  });
  window.shellShow = async () => {
    input.value = ""; all = false; sel = 0;
    await Promise.all([loadApps(), loadStatus()]);
    $("span", user).textContent = status ? `${status.user} · ${status.host}` : "";
    draw();
    input.focus();
  };
  window.shellShow();
}

// ── widgets (left) ──────────────────────────────────────────────────────────────────────────
function widgets() {
  document.body.className = "fly left";
  const body = el("div", { class: "scroll" });
  root.append(el("div", { class: "panel" }, el("div", { class: "head" }, el("span", { class: "al", text: "WIDGETS" }), el("span", { class: "jp", text: "小道具" }), el("span", { class: "rule" })), body));
  const draw = () => {
    const d = new Date();
    const kids = [];
    kids.push(el("div", { class: "clockbig" },
      el("div", { class: "t", text: `${pad(d.getHours())}:${pad(d.getMinutes())}` }),
      el("div", { class: "d" }, `${WEEK_EN[d.getDay()]} ${d.getDate()} ${d.toLocaleString("en", { month: "long" }).toUpperCase()} ${d.getFullYear()}`,
        el("span", { class: "jp", text: `${d.getMonth() + 1}月${d.getDate()}日 (${WEEK_JP[d.getDay()]})` }))));
    // the month
    const cal = el("div", { class: "cal" }, ...WEEK_JP.map((w, i) => el("div", { class: "h", text: `${w} ${WEEK_EN[i][0]}` })));
    const first = new Date(d.getFullYear(), d.getMonth(), 1);
    const startAt = new Date(first); startAt.setDate(1 - first.getDay());
    for (let i = 0; i < 42; i++) {
      const c = new Date(startAt); c.setDate(startAt.getDate() + i);
      if (i >= 35 && c.getMonth() !== d.getMonth()) break;
      cal.append(el("div", { class: "c" + (c.getMonth() !== d.getMonth() ? " o" : "") + (c.toDateString() === d.toDateString() ? " today" : ""), text: String(c.getDate()) }));
    }
    kids.push(el("div", { class: "sect" }, el("span", { class: "al", text: "暦 CALENDAR" }), el("span", { class: "rule" })), cal);
    // update
    if (status && status.update && status.update.ready) {
      kids.push(el("div", { class: "sect" }, el("span", { class: "al", text: "更新 UPDATE" }), el("span", { class: "rule" })),
        el("div", { class: "card" }, el("div", { class: "k" }, el("span", { text: "Update ready — install?" }), el("span", { text: `${status.update.version || ""} · ${status.update.files} files` })),
          el("button", { class: "act", text: "INSTALL NOW · 今すぐ", on: { click: () => { post("/api/quick", { what: "open", app: "update" }); panel({ do: "hide" }); } } })));
    }
    // system
    if (status) {
      const b = status.battery || {}, n = status.network || {};
      const card = el("div", { class: "card" });
      if (b.percent != null) card.append(el("div", { class: "k" }, el("span", { text: "電池 battery" }), el("span", { class: "big", text: `${b.percent}%` })), el("div", { class: "bar" }, el("i", { style: `width:${b.percent}%` })), el("div", { class: "k" }, el("span", { text: b.charging ? "charging" : b.ac ? "plugged in" : "on battery" }), el("span")));
      card.append(el("div", { class: "k" }, el("span", { text: "通信 network" }), el("span", { text: n.wired ? "wired" : n.wifi ? `${n.wifi.ssid} · ${n.wifi.signal}%` : n.wifi_on ? "not connected" : "Wi-Fi off" })));
      card.append(el("div", { class: "k" }, el("span", { text: "機体 host" }), el("span", { text: `${status.user}@${status.host}` })));
      kids.push(el("div", { class: "sect" }, el("span", { class: "al", text: "系統 SYSTEM" }), el("span", { class: "rule" })), card);
    }
    // workspaces of every screen
    if (shell && shell.screens.length) {
      const g = el("div", { class: "wsgrid" });
      for (const s of shell.screens) {
        g.append(el("div", { class: "wsrow" }, el("span", { class: "nm", text: (s.primary ? "◆ " : "◇ ") + s.name }),
          ...s.workspaces.map(w => el("button", { class: "wsdot" + (w.windows ? " has" : "") + (w.active ? " on" : ""), text: w.windows ? String(w.windows) : KANJI[w.n],
            on: { click: () => post("/api/shell", { do: "workspace", screen: s.index, n: w.n }) } }))));
      }
      kids.push(el("div", { class: "sect" }, el("span", { class: "al", text: "空間 WORKSPACES" }), el("span", { class: "rule" })), el("div", { class: "card" }, g));
    }
    body.replaceChildren(...kids);
  };
  onShell(draw);
  document.addEventListener("keydown", e => { if (e.key === "Escape") panel({ do: "hide" }); });
  window.shellShow = async () => { await loadStatus(); draw(); };
  setInterval(draw, 15000);
  window.shellShow();
}

// ── quick settings (right) ──────────────────────────────────────────────────────────────────
function quick() {
  document.body.className = "fly right";
  const tiles = el("div", { class: "tiles" });
  const sliders = el("div");
  const foot = el("div", { class: "qfoot" });
  root.append(el("div", { class: "panel" }, el("div", { class: "head" }, el("span", { class: "al", text: "QUICK" }), el("span", { class: "jp", text: "設定" }), el("span", { class: "rule" })), tiles, sliders, foot));
  const tile = (g, name, sub, on, click, na) => el("button", { class: "tile" + (on ? " on" : "") + (na ? " na" : ""), on: { click: na ? () => {} : click } }, glyph(g), el("span", { class: "n", text: name }), el("span", { class: "s", text: sub }));
  let dragging = false;
  const slider = (g, label, v, what) => {
    const out = el("span", { class: "v", text: v == null ? "—" : `${v}` });
    const r = el("input", { type: "range", min: "0", max: "100", value: String(v || 0) });
    if (v == null) r.disabled = true;
    let t = 0;
    r.addEventListener("input", () => { out.textContent = r.value; dragging = true; clearTimeout(t); t = setTimeout(() => { post("/api/quick", { what, value: +r.value }); dragging = false; }, 60); });
    return el("div", { class: "slider" }, glyph(g), r, out);
  };
  const draw = () => {
    if (!status || dragging) return;
    const n = status.network || {}, s = status.sound || {}, bt = status.bluetooth, b = status.battery || {};
    tiles.replaceChildren(
      tile("network", "通信 Wi-Fi", n.wifi ? n.wifi.ssid : n.wifi_on ? "on" : "off", n.wifi_on, () => post("/api/quick", { what: "wifi", on: !n.wifi_on }).then(refresh)),
      tile("system", "無線 Bluetooth", bt ? (bt.on ? "on" : "off") : "not here", bt && bt.on, () => post("/api/quick", { what: "bluetooth", on: !(bt && bt.on) }).then(refresh), !bt),
      tile("power", "機内 Airplane", n.airplane ? "on" : "off", n.airplane, () => post("/api/quick", { what: "airplane", on: !n.airplane }).then(refresh)),
      tile("volume", "消音 Mute", s.muted ? "muted" : "sound on", s.muted, () => post("/api/quick", { what: "mute" }).then(refresh)),
      tile("network", "接続 Networks", "open the manager", false, () => { post("/api/quick", { what: "open", app: "network" }); panel({ do: "hide" }); }),
      tile("update", "更新 Updates", status.update && status.update.ready ? "ready to install" : "check", status.update && status.update.ready, () => { post("/api/quick", { what: "open", app: status.update && status.update.ready ? "update" : "updates" }); panel({ do: "hide" }); }),
    );
    sliders.replaceChildren(slider("brightness", "輝度", status.brightness, "brightness"), slider("volume", "音量", s.volume, "volume"));
    foot.replaceChildren(
      el("div", { class: "bat" }, glyph("battery"), b.percent != null ? el("b", { text: `${b.percent}%` }) : el("span", { text: "AC" }), el("span", { class: "jp", text: b.charging ? "充電中" : "" })),
      el("div", {},
        el("button", { class: "pbtn", on: { click: () => { post("/api/quick", { what: "open", app: "settings" }); panel({ do: "hide" }); } } }, glyph("settings"), el("span", { class: "jp", text: "設定" })),
        el("button", { class: "pbtn", on: { click: () => { panel({ do: "hide" }); post("/api/power", { what: "lock" }); } } }, glyph("lock"), el("span", { class: "jp", text: "施錠" }))));
  };
  const refresh = async () => { await loadStatus(); draw(); };
  document.addEventListener("keydown", e => { if (e.key === "Escape") panel({ do: "hide" }); });
  window.shellShow = refresh;
  setInterval(refresh, 5000);
  refresh();
}

// the panel never acts like a web page: no context menu, no zoom, no drag-and-drop navigation
document.addEventListener("contextmenu", e => e.preventDefault());
document.addEventListener("wheel", e => { if (e.ctrlKey) e.preventDefault(); }, { passive: false });
document.addEventListener("keydown", e => { if ((e.ctrlKey && ["+", "-", "=", "0", "r", "p", "s", "u"].includes(e.key.toLowerCase())) || e.key === "F5") e.preventDefault(); });
document.addEventListener("dragover", e => e.preventDefault());
document.addEventListener("drop", e => e.preventDefault());

(async () => {
  await unlock();
  shellStream();
  ({ taskbar, start, widgets, quick }[ROLE] || taskbar)();
})();
