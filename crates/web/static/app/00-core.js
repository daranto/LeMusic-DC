"use strict";

// app/00-core.js – Hilfsfunktionen, API-Zugriff, Zustand, Login, Tabs.
// Wird mit den anderen Dateien in app/ zu /app.js zusammengesetzt (assets.rs).

// ---------------------------------------------------------------------------
// Hilfsfunktionen
// ---------------------------------------------------------------------------

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

function fmt(secs) {
  if (secs == null) return "Live";
  secs = Math.max(0, Math.floor(secs));
  const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), s = secs % 60;
  const ss = String(s).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** Hinweis unten rechts; optional mit Knopf (z. B. „Rückgängig“). */
function toast(msg, kind = "", act) {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.append(msg);
  if (act) {
    const b = document.createElement("button");
    b.className = "btn small";
    b.textContent = act.label;
    b.addEventListener("click", () => {
      el.remove();
      act.fn();
    });
    el.append(b);
  }
  $("#toasts").append(el);
  setTimeout(() => el.remove(), act ? 10000 : kind === "error" ? 6000 : 3500);
}

/** Link in die Zwischenablage (ohne HTTPS nicht überall erlaubt). */
async function copyLink(url) {
  try {
    await navigator.clipboard.writeText(url);
    toast("Link kopiert", "ok");
  } catch {
    prompt("Link kopieren:", url);
  }
}

class Unauthorized extends Error {}

async function api(method, path, body, raw = false) {
  const opts = { method, headers: { "X-LeMusic": "1" }, credentials: "same-origin" };
  if (body !== undefined) {
    if (raw) {
      opts.body = body;
      opts.headers["Content-Type"] = "text/plain";
    } else {
      opts.body = JSON.stringify(body);
      opts.headers["Content-Type"] = "application/json";
    }
  }
  const res = await fetch(`/api${path}`, opts);
  let data = null;
  try { data = await res.json(); } catch { /* leere Antwort */ }
  if (res.status === 401 && path !== "/login") {
    showLogin();
    throw new Unauthorized();
  }
  if (!res.ok) {
    const err = new Error(data?.error || `Fehler ${res.status}`);
    err.status = res.status;
    throw err;
  }
  if (data?.queue_full) toast(`${data.queue_full} Titel passten nicht mehr in die Warteschlange`, "error");
  return data;
}

/** Führt eine Aktion aus, sperrt den Button und meldet Fehler. */
async function run(button, fn, okMsg) {
  if (button) button.disabled = true;
  try {
    const r = await fn();
    if (okMsg) toast(typeof okMsg === "function" ? okMsg(r) : okMsg, "ok");
    return r;
  } catch (e) {
    if (!(e instanceof Unauthorized)) toast(e.message, "error");
  } finally {
    if (button) button.disabled = false;
  }
}

function formData(form) {
  const out = {};
  for (const el of form.elements) {
    if (!el.name) continue;
    if (el.type === "checkbox") out[el.name] = el.checked;
    else if (el.type === "number") out[el.name] = el.value === "" ? null : Number(el.value);
    else out[el.name] = el.value;
  }
  return out;
}

function fillSelect(select, items, { empty, selected } = {}) {
  select.replaceChildren();
  if (empty !== undefined) select.append(new Option(empty, ""));
  for (const it of items) select.append(new Option(it.name, it.id));
  if (selected != null) select.value = selected;
}

// ---------------------------------------------------------------------------
// Zustand
// ---------------------------------------------------------------------------

const state = {
  guild: null,
  player: null,
  playerFetchedAt: 0,
  tab: "dashboard",
  logSeq: 0,
  playlists: [],
  autoplaylist: null,
  timers: [],
};

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

function showLogin() {
  state.timers.forEach(clearInterval);
  state.timers = [];
  $("#app-view").hidden = true;
  $("#login-view").hidden = false;
  $("#login-form input[name=username]").focus();
}

async function showApp(username) {
  $("#login-view").hidden = true;
  $("#app-view").hidden = false;
  $("#username").textContent = username;
  await refreshOverview();
  await refreshPlayer();
  loadPlaylists();
  api("GET", "/settings/guild").then((g) => (state.autoplaylist = g.autoplaylist)).catch(() => {});
  state.timers.push(setInterval(refreshPlayer, 2000));
  state.timers.push(setInterval(refreshOverview, 15000));
  state.timers.push(setInterval(tickProgress, 1000));
  state.timers.push(setInterval(() => state.tab === "logs" && refreshLogs(), 2000));
}

$("#login-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const btn = $("button", ev.target);
  btn.disabled = true;
  $("#login-error").textContent = "";
  try {
    const r = await api("POST", "/login", formData(ev.target));
    ev.target.reset();
    await showApp(r.username);
  } catch (e) {
    $("#login-error").textContent = e.message;
  } finally {
    btn.disabled = false;
  }
});

$("#logout").addEventListener("click", () =>
  run(null, async () => {
    await api("POST", "/logout");
    showLogin();
  }));

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

function switchTab(tab) {
  state.tab = tab;
  $$(".tabs button").forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
  $$(".tab").forEach((s) => (s.hidden = s.id !== `tab-${tab}`));
  if (tab === "playlists") loadPlaylists();
  if (tab === "settings") loadSettings();
  if (tab === "logs") refreshLogs();
  history.replaceState(null, "", `#${tab}`);
}
$$(".tabs button").forEach((b) => b.addEventListener("click", () => switchTab(b.dataset.tab)));
