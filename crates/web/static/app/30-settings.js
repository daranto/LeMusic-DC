// app/30-settings.js – Einstellungen (Bot, Server).
// Wird mit den anderen Dateien in app/ zu /app.js zusammengesetzt (assets.rs).

// ---------------------------------------------------------------------------
// Einstellungen
// ---------------------------------------------------------------------------

async function loadSettings() {
  const [bot, guild, lists] = await Promise.all([
    run(null, () => api("GET", "/settings/bot")),
    run(null, () => api("GET", "/settings/guild")),
    run(null, () => api("GET", "/playlists")),
  ]);
  if (bot) {
    const f = $("#bot-settings");
    f.status_text.value = bot.status_text;
    f.default_volume.value = bot.default_volume;
    f.idle_timeout_secs.value = bot.idle_timeout_secs;
    f.pause_when_alone.checked = bot.pause_when_alone;
    f.fair_queue.checked = bot.fair_queue;
    f.normalize_volume.checked = bot.normalize_volume;
    f.public_enabled.checked = bot.public_enabled;
    f.public_pin.value = bot.public_pin;
    f.public_url.value = bot.public_url;
    f.ytdlp_auto_update.checked = bot.ytdlp_auto_update;
    f.ytdlp_update_time.value = bot.ytdlp_update_time;
    f.cache_enabled.checked = bot.cache_enabled;
    f.cache_max_gb.value = Math.round((bot.cache_max_mb / 1024) * 10) / 10;
    updateCacheField();
    updatePublicNote();
    updateYtdlpNote();
  }
  if (guild) {
    const f = $("#guild-settings");
    const g = state.guild;
    fillSelect(f.dj_role, g?.roles ?? [], { empty: "— keine (alle sind DJ) —", selected: guild.dj_role ?? "" });
    fillSelect(f.text_channel, g?.text_channels ?? [], { empty: "— alle Kanäle —", selected: guild.text_channel ?? "" });
    fillSelect(f.announce_channel, g?.text_channels ?? [], {
      empty: "— Kanal des letzten Befehls —",
      selected: guild.announce_channel ?? "",
    });
    fillSelect(f.voice_channel, g?.voice_channels ?? [], { empty: "— alle Kanäle —", selected: guild.voice_channel ?? "" });
    fillSelect(f.autoplaylist, (lists ?? []).map((p) => ({ id: p.name, name: p.name })), {
      empty: "— keine —",
      selected: guild.autoplaylist ?? "",
    });
    f.skip_ratio.value = guild.skip_ratio;
    f.max_track_mins.value = Math.round(guild.max_track_secs / 60);
    f.announce_tracks.checked = guild.announce_tracks;
    f.auto_join.checked = guild.auto_join;
    const picks = $("#auto-join-channels .picks");
    const chosen = new Set(guild.auto_join_channels ?? []);
    picks.innerHTML = (g?.voice_channels ?? []).map((c) => `
      <label class="check"><input type="checkbox" data-channel="${esc(c.id)}" ${chosen.has(c.id) ? "checked" : ""}> ${esc(c.name)}</label>`).join("")
      || `<span class="muted">Sprachkanäle erscheinen, sobald der Bot mit Discord verbunden ist.</span>`;
    updateAutoJoin();
  }
}

function fmtBytes(b) {
  if (b >= 1024 ** 3) return `${(b / 1024 ** 3).toFixed(1).replace(".", ",")} GB`;
  return `${Math.round(b / 1024 ** 2)} MB`;
}

function renderCache(c) {
  if (!c) return;
  const badge = $("#cache-badge");
  badge.textContent = c.enabled ? "aktiv" : "aus";
  badge.classList.toggle("on", c.enabled);
  $("#cache-usage").textContent = c.entries
    ? `${fmtBytes(c.bytes)} von ${fmtBytes(c.max_bytes)} belegt · ${c.entries} Titel`
    : c.enabled ? `Noch leer · Platz für ${fmtBytes(c.max_bytes)}` : "Leer";
  $("#cache-bar").style.width = c.max_bytes ? `${Math.min(100, (c.bytes / c.max_bytes) * 100)}%` : "0%";
  const total = c.hits + c.misses;
  $("#cache-hits").textContent = total
    ? `Seit dem Start: ${c.hits} aus dem Cache, ${c.misses} neu geladen (${Math.round((c.hits / total) * 100)} % Treffer)`
    : "";
}

function updateCacheField() {
  const f = $("#bot-settings");
  f.cache_max_gb.disabled = !f.cache_enabled.checked;
}
$("#bot-settings [name=cache_enabled]").addEventListener("change", updateCacheField);

$("#btn-cache-clear").addEventListener("click", (ev) => {
  if (!confirm("Alle zwischengespeicherten Titel löschen? Sie werden beim nächsten Abspielen neu geladen.")) return;
  run(ev.currentTarget, () => api("DELETE", "/system/cache"), (r) => `${r.removed} Titel gelöscht`).then(refreshOverview);
});

function renderYtdlpLast(s) {
  const el = $("#ytdlp-last");
  el.textContent = s
    ? `Letzte Prüfung (${s.trigger}): ${s.at} – ${s.ok ? "✅" : "❌"} ${s.message}`
    : "Noch keine Prüfung seit dem Update auf diese Version.";
}

function updateYtdlpNote() {
  const f = $("#bot-settings");
  f.ytdlp_update_time.disabled = !f.ytdlp_auto_update.checked;
  $("#ytdlp-schedule-note").textContent = f.ytdlp_auto_update.checked
    ? `Zeitzone: ${state.timeZone || "Europe/Berlin"} (änderbar über die Umgebungsvariable TZ). Zusätzlich wird bei jedem Containerstart aktualisiert.`
    : "Aus – dann nur beim Containerstart oder über System → „Jetzt aktualisieren“.";
}
$("#bot-settings [name=ytdlp_auto_update]").addEventListener("change", updateYtdlpNote);

function updateAutoJoin() {
  $("#auto-join-channels").disabled = !$("#guild-settings").auto_join.checked;
}
$("#guild-settings [name=auto_join]").addEventListener("change", updateAutoJoin);

function updatePublicNote() {
  const f = $("#bot-settings");
  const base = (f.public_url.value.trim() || location.origin).replace(/\/+$/, "");
  const note = $("#public-link-note");
  note.replaceChildren();
  if (!f.public_enabled.checked) {
    note.textContent = "Deaktiviert.";
    return;
  }
  const a = document.createElement("a");
  a.href = `${base}/public`;
  a.target = "_blank";
  a.rel = "noopener";
  a.textContent = `${base}/public`;
  note.append("Link: ", a);
  if (!f.public_url.value.trim()) note.append(" – ohne öffentliche Adresse postet der Bot keine Links in Discord.");
}
["public_enabled", "public_url"].forEach((n) =>
  $(`#bot-settings [name=${n}]`).addEventListener("input", updatePublicNote));

$("#bot-settings").addEventListener("submit", (ev) => {
  ev.preventDefault();
  const body = formData(ev.target);
  body.cache_max_mb = Math.round((body.cache_max_gb ?? 2) * 1024);
  delete body.cache_max_gb;
  run($("button", ev.target), () => api("PUT", "/settings/bot", body), "Gespeichert").then(refreshOverview);
});

$("#guild-settings").addEventListener("submit", (ev) => {
  ev.preventDefault();
  const d = formData(ev.target);
  const body = {
    dj_role: d.dj_role || null,
    text_channel: d.text_channel || null,
    announce_channel: d.announce_channel || null,
    voice_channel: d.voice_channel || null,
    skip_ratio: d.skip_ratio ?? 55,
    max_track_secs: (d.max_track_mins ?? 0) * 60,
    autoplaylist: d.autoplaylist || null,
    announce_tracks: d.announce_tracks,
    auto_join: d.auto_join,
    auto_join_channels: $$("#auto-join-channels input[data-channel]:checked").map((i) => i.dataset.channel),
  };
  run($("button", ev.target), () => api("PUT", "/settings/guild", body), "Gespeichert").then((r) => {
    if (r) state.autoplaylist = body.autoplaylist;
  });
});
