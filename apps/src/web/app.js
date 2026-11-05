// Shadow Settings — the web app over shadow-webd's local API.
// The key arrives once in the URL fragment (#k=…&p=…); it is traded for a session cookie and
// removed from the address bar straight away.
"use strict";
const $ = (s, el = document) => el.querySelector(s);
const page = $("#page");
let current = "settings";

function status(msg, kind = "") {
  const f = $("#status");
  f.textContent = msg;
  f.className = kind;
}

async function api(method, path, body) {
  const r = await fetch(path, {
    method, credentials: "same-origin",
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const j = await r.json().catch(() => ({ error: "bad reply" }));
  if (r.status === 401) { locked(); throw new Error(j.error); }
  if (!r.ok) throw new Error(j.error || r.statusText);
  return j;
}

function el(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "on") for (const [ev, fn] of Object.entries(v)) e.addEventListener(ev, fn);
    else if (k === "text") e.textContent = v;
    else e.setAttribute(k, v);
  }
  for (const k of kids) if (k != null) e.append(k);
  return e;
}

function locked() {
  page.replaceChildren($("#locked").content.cloneNode(true));
  status("locked", "err");
}

// ── kinetic unfolding: a tessellation over the page folds away from the click ──────────────────
function unfold(ox = 0.5, oy = 0) {
  const t = $("#tess");
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const r = t.getBoundingClientRect();
  const cols = Math.max(3, Math.min(9, Math.round(r.width / 150)));
  const rows = Math.max(2, Math.min(7, Math.round(Math.min(r.height, innerHeight) / 150)));
  t.style.setProperty("grid-template-columns", `repeat(${cols}, 1fr)`);
  t.style.setProperty("grid-template-rows", `repeat(${rows}, ${Math.min(r.height, innerHeight) / rows}px)`);
  t.replaceChildren();
  const far = Math.hypot(Math.max(ox, 1 - ox), Math.max(oy, 1 - oy));
  for (let j = 0; j < rows; j++) for (let i = 0; i < cols; i++) {
    const cx = (i + .5) / cols, cy = (j + .5) / rows;
    const dx = cx - ox, dy = cy - oy;
    const d = Math.hypot(dx, dy) / far;
    const c = el("div", { class: "cell" });
    c.style.setProperty("--d", `${(d * 0.32).toFixed(3)}s`);
    // the cell swings shut toward its far edge, hinged on the side facing the origin
    if (Math.abs(dx) * rows > Math.abs(dy) * cols) {
      c.style.setProperty("--o", dx > 0 ? "right center" : "left center");
      c.style.setProperty("--sx", "0"); c.style.setProperty("--sy", "1");
    } else {
      c.style.setProperty("--o", dy > 0 ? "center bottom" : "center top");
      c.style.setProperty("--sx", "1"); c.style.setProperty("--sy", "0");
    }
    t.append(c);
  }
  clearTimeout(unfold.t);
  unfold.t = setTimeout(() => t.replaceChildren(), 1000);
}

// ── pages ───────────────────────────────────────────────────────────────────────────────────────
async function settingsPage() {
  const pages = await api("GET", "/api/settings");
  const out = [];
  for (const p of pages) {
    out.push(el("h2", { text: p.name }), el("p", { class: "about", text: p.about }));
    for (const r of p.rows) out.push(settingRow(r));
  }
  page.replaceChildren(...out);
}

function settingRow(r) {
  const k = r.kind, val = r.value ?? r.default;
  const save = async v => {
    try { const j = await api("POST", "/api/settings", { section: r.section, key: r.key, value: v }); status(`${r.label}: ${j.ok}`, "ok"); }
    catch (e) { status(`${r.label}: ${e.message}`, "err"); }
  };
  let input;
  if (k.type === "bool") {
    input = el("input", { type: "checkbox", on: { change: e => save(e.target.checked) } });
    input.checked = String(val) === "true";
  } else if (k.type === "choice") {
    input = el("select", { on: { change: e => save(e.target.value) } }, ...k.choices.map(c => el("option", { value: c, text: c })));
    input.value = val;
  } else if (k.type === "num") {
    const box = el("input", { type: "number", min: k.min, max: k.max, step: k.step });
    const rng = el("input", { type: "range", min: k.min, max: k.max, step: k.step });
    box.value = rng.value = val;
    rng.addEventListener("input", () => box.value = rng.value);
    rng.addEventListener("change", () => save(Number(rng.value)));
    box.addEventListener("change", () => { rng.value = box.value; save(Number(box.value)); });
    input = el("div", { class: "num" }, rng, box);
  } else if (k.type === "color") {
    input = el("input", { type: "color", on: { change: e => save(e.target.value.toUpperCase()) } });
    input.value = /^#[0-9a-fA-F]{6}$/.test(val) ? val : r.default;
  } else {
    input = el("input", { type: "text", on: { change: e => save(e.target.value) } });
    input.value = val ?? "";
  }
  return el("div", { class: "row" }, el("div", {}, el("label", { text: r.label }), el("div", { class: "def", text: `${r.section}.${r.key} · default ${r.default || "—"}` })), input);
}

async function fixPage() {
  const [issues, tasks] = await Promise.all([api("GET", "/api/fix/issues"), api("GET", "/api/fix/tasks")]);
  const out = [el("h2", { text: "ISSUES" }), el("p", { class: "about", text: "known problems of your apps on this desktop; repairs keep a .shadow-bak copy, and ones that need your password open a terminal" })];
  issues.sort((a, b) => b.issue - a.issue);
  if (!issues.length) out.push(el("div", { class: "item" }, el("span", { class: "lvl", text: "·" }), el("div", { class: "t", text: "nothing detected" })));
  for (const i of issues) {
    const btn = i.issue ? el("button", { class: "act", text: i.root ? "REPAIR ⚿" : "REPAIR", on: { click: async () => {
      try { const j = await api("POST", "/api/fix/apply", { id: i.id, root: i.root }); status(j.ok, "ok"); fixPage(); } catch (e) { status(e.message, "err"); }
    } } }) : null;
    out.push(el("div", { class: "item" + (i.issue ? " bad" : "") }, el("span", { class: "lvl", text: i.issue ? "!" : "✓" }), el("div", {}, el("div", { class: "t", text: i.title }), el("div", { class: "d", text: i.detail })), btn));
  }
  out.push(el("h2", { text: "TASKS" }), el("p", { class: "about", text: "everyday “it’s stuck” fixes: caches, profile locks, force quit, portals" }));
  for (const t of tasks) out.push(el("div", { class: "item" }, el("span", { class: "lvl", text: "▸" }), el("div", { class: "t", text: t.label }),
    el("button", { class: "act", text: "RUN", on: { click: async () => { try { status((await api("POST", "/api/fix/run", { id: t.id })).ok || "done", "ok"); } catch (e) { status(e.message, "err"); } } } })));
  out.push(el("h2", { text: "CONSOLES" }));
  for (const [what, label, about] of [["passfix", "PASSWORD REPAIR", "lockouts, keyboard layout, PAM, keyring — in a terminal"], ["update", "UPDATE + TRIAGE", "update the system, then check what the update changed"]])
    out.push(el("div", { class: "item" }, el("span", { class: "lvl", text: "◇" }), el("div", {}, el("div", { class: "t", text: label }), el("div", { class: "d", text: about })),
      el("button", { class: "act", text: "OPEN", on: { click: async () => { try { status((await api("POST", "/api/fix/console", { what })).ok, "ok"); } catch (e) { status(e.message, "err"); } } } })));
  page.replaceChildren(...out);
}

async function defaultsPage() {
  const d = await api("GET", "/api/defaults");
  const pick = (label, list, cur, key) => {
    const s = el("select", {}, el("option", { value: "", text: "automatic" }), ...list.map(t => el("option", { value: t, text: t })));
    s.value = list.includes(cur) ? cur : "";
    const custom = el("input", { type: "text", placeholder: "or a command, e.g. foot -e btop" });
    if (cur && !list.includes(cur)) custom.value = cur;
    const save = async v => { try { status((await api("POST", "/api/defaults", { [key]: v })).ok, "ok"); } catch (e) { status(e.message, "err"); } };
    s.addEventListener("change", () => { custom.value = ""; save(s.value); });
    custom.addEventListener("change", () => save(custom.value));
    return el("div", { class: "row" }, el("div", {}, el("label", { text: label }), el("div", { class: "def", text: `defaults.${key}` })), el("div", {}, s, custom));
  };
  page.replaceChildren(el("h2", { text: "DEFAULT APPS" }), el("p", { class: "about", text: "what Mod+Return and Mod+E open; the terminal also runs console repairs" }),
    pick("Terminal", d.terminals, d.terminal, "terminal"), pick("File manager", d.file_managers, d.files, "files"));
}

async function configPage() {
  const c = await api("GET", "/api/config");
  const ta = el("textarea", { spellcheck: "false" });
  ta.value = c.text;
  const save = el("button", { class: "act", text: "SAVE + APPLY", on: { click: async () => {
    try { status((await api("PUT", "/api/config", { text: ta.value })).ok + " · the previous file is config.toml.bak", "ok"); } catch (e) { status(e.message, "err"); }
  } } });
  page.replaceChildren(el("h2", { text: "CONFIG.TOML" }), el("p", { class: "about", text: "the whole file; it is checked before it is saved, and the previous one is kept as config.toml.bak" }), ta, el("p", {}, save));
}

async function crashPage() {
  const lines = await api("GET", "/api/crash");
  const rep = el("button", { class: "act", text: "SAVE A CRASH REPORT", on: { click: async () => {
    try { status("report saved: " + (await api("POST", "/api/crash/report")).ok + " — upload it yourself; nothing is sent", "ok"); } catch (e) { status(e.message, "err"); }
  } } });
  const log = el("div", { class: "log" });
  log.textContent = lines.length ? lines.map(l => `${l.file}: ${l.line}`).join("\n") : "no crashes or errors logged";
  page.replaceChildren(el("h2", { text: "CRASH LOG" }), el("p", { class: "about", text: "errors from Shadow's own programs; the report is a flat file with names, networks and keys redacted" }), log, el("p", {}, rep));
}

const PAGES = { settings: settingsPage, fix: fixPage, defaults: defaultsPage, config: configPage, crash: crashPage };

async function show(p, ox, oy) {
  if (!PAGES[p]) p = "settings";
  current = p;
  for (const b of document.querySelectorAll("#tabs button")) b.classList.toggle("on", b.dataset.p === p);
  status("…");
  try { await PAGES[p](); status("ready"); unfold(ox, oy); } catch (e) { if (!/locked|session/.test(e.message)) status(e.message, "err"); }
  history.replaceState(null, "", "/#p=" + p);
}

async function start() {
  const h = new URLSearchParams(location.hash.slice(1));
  const key = h.get("k");
  if (key) {
    await fetch("/api/unlock", { method: "POST", credentials: "same-origin", headers: { "X-Shadow-Key": key } }).catch(() => {});
    history.replaceState(null, "", "/#p=" + (h.get("p") || "settings")); // the key leaves the address bar
  }
  for (const b of document.querySelectorAll("#tabs button")) b.addEventListener("click", e => {
    const r = $("#main").getBoundingClientRect();
    show(b.dataset.p, (e.clientX - r.left) / r.width, 0);
  });
  try {
    const i = await api("GET", "/api/info");
    $("#who").textContent = `${i.host} · ${i.os} · Shadow ${i.version}`;
  } catch { return; }
  show(h.get("p") || "settings");
  if ("serviceWorker" in navigator) navigator.serviceWorker.register("/sw.js").catch(() => {});
}
start();
