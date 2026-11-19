// app/40-system.js – System (yt-dlp, Cache, Backup, Selbsttest, Cookies), Logs, Konto, Start.
// Wird mit den anderen Dateien in app/ zu /app.js zusammengesetzt (assets.rs).

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

$("#btn-ytdlp-update").addEventListener("click", (ev) => {
  const btn = ev.currentTarget;
  btn.textContent = "Aktualisiere …";
  run(btn, () => api("POST", "/system/ytdlp-update"), (r) => `yt-dlp ${r.version} – ${r.status.message}`).finally(() => {
    btn.textContent = "Jetzt aktualisieren";
    refreshOverview();
  });
});

$("#selftest-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const out = $("#selftest-result");
  out.hidden = false;
  out.textContent = "Teste … (ca. 15–25 Sekunden)";
  const r = await run($("button", ev.target), () => api("POST", "/system/selftest", formData(ev.target)));
  if (!r) {
    out.textContent = "Fehlgeschlagen – Details in der Meldung unten rechts.";
    return;
  }
  const t = r.report;
  out.textContent =
    `${r.ok ? "✅ OK" : "❌ Aussetzer erkannt"}\n` +
    `Track:        ${t.title} – ${t.author}\n` +
    `Auflösen:     ${t.resolve_ms} ms\n` +
    `Stream offen: ${t.open_ms} ms\n` +
    `Gelesen:      ${t.decoded_secs.toFixed(1)} s, langsamster Lesevorgang ${t.slowest_read_ms.toFixed(1)} ms, Blockaden: ${t.stalls}`;
});

$("#btn-cookies-upload").addEventListener("click", async (ev) => {
  const file = $("#cookies-file").files[0];
  if (!file) return toast("Bitte zuerst eine cookies.txt auswählen", "error");
  const text = await file.text();
  run(ev.currentTarget, () => api("PUT", "/system/cookies", text, true), "Cookies gespeichert").then(refreshOverview);
});
$("#btn-cookies-delete").addEventListener("click", (ev) =>
  run(ev.currentTarget, () => api("DELETE", "/system/cookies"), "Cookies entfernt").then(refreshOverview));

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

const LEVELS = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"];
let logLines = [];

async function refreshLogs() {
  try {
    const lines = await api("GET", `/logs?after=${state.logSeq}`);
    if (lines.length) {
      state.logSeq = lines[lines.length - 1].seq;
      logLines = logLines.concat(lines).slice(-1000);
    }
    renderLogs();
  } catch (e) {
    if (!(e instanceof Unauthorized)) console.warn(e);
  }
}

function renderLogs() {
  const min = LEVELS.indexOf($("#log-level").value);
  const box = $("#logs");
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
  box.innerHTML = logLines
    .filter((l) => LEVELS.indexOf(l.level) >= min)
    .map((l) => {
      const t = new Date(l.ts).toLocaleTimeString("de-DE");
      return `<div class="${esc(l.level)}"><span class="t">${t}</span> ${esc(l.level.padEnd(5))} ${esc(l.message)}</div>`;
    })
    .join("") || `<div class="t">Keine Einträge.</div>`;
  if (atBottom) box.scrollTop = box.scrollHeight;
}
$("#log-level").addEventListener("change", renderLogs);

// ---------------------------------------------------------------------------
// Konto
// ---------------------------------------------------------------------------

$("#password-form").addEventListener("submit", (ev) => {
  ev.preventDefault();
  run($("button", ev.target), () => api("POST", "/account/password", formData(ev.target)), "Passwort geändert").then(
    (r) => r && ev.target.reset());
});

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

(async () => {
  try {
    const me = await api("GET", "/me");
    await showApp(me.username);
    const tab = location.hash.slice(1);
    if (tab && $(`#tab-${CSS.escape(tab)}`)) switchTab(tab);
  } catch (e) {
    if (!(e instanceof Unauthorized)) showLogin();
  }
})();
