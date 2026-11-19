// app/10-player.js – Übersicht, Wiedergabe, Warteschlange.
// Wird mit den anderen Dateien in app/ zu /app.js zusammengesetzt (assets.rs).

// ---------------------------------------------------------------------------
// Übersicht & Wiedergabe
// ---------------------------------------------------------------------------

async function refreshOverview() {
  try {
    const o = await api("GET", "/overview");
    state.guild = o.guild;
    const chip = (label, ok) =>
      `<span class="chip"><span class="dot ${ok ? "ok" : "bad"}"></span>${esc(label)}</span>`;
    const up = o.uptime_secs;
    const uptime = up > 86400 ? `${Math.floor(up / 86400)} T ${Math.floor((up % 86400) / 3600)} h` : fmt(up);
    $("#status-row").innerHTML =
      chip(`Discord${o.guild ? ` · ${o.guild.name}` : ""}`, o.health.discord) +
      chip("Datenbank", o.health.database) +
      chip("Player", o.health.player) +
      chip("Voice", o.health.voice) +
      chip(`yt-dlp ${o.ytdlp_version}`, o.ytdlp_version !== "nicht verfügbar") +
      `<span class="chip">⏱ Laufzeit ${esc(uptime)}</span>` +
      `<span class="chip">v${esc(o.health.version)}</span>`;
    $("#ytdlp-version").textContent = o.ytdlp_version;
    state.timeZone = o.time_zone;
    renderYtdlpLast(o.ytdlp_last_update);
    renderCache(o.cache);
    const badge = $("#cookies-badge");
    badge.textContent = o.cookies ? "aktiv" : "keine";
    badge.classList.toggle("on", o.cookies);
    if (o.guild) {
      const sel = $("#voice-select");
      const prev = sel.value;
      fillSelect(sel, o.guild.voice_channels, { empty: "— aktueller Kanal des Bots —", selected: prev });
    }
  } catch (e) {
    if (!(e instanceof Unauthorized)) console.warn(e);
  }
}

async function refreshPlayer() {
  try {
    state.player = await api("GET", "/player");
    state.playerFetchedAt = Date.now();
    renderPlayer();
  } catch (e) {
    if (!(e instanceof Unauthorized)) console.warn(e);
  }
}

function channelName(id) {
  return state.guild?.voice_channels.find((c) => c.id === id)?.name;
}

function renderPlayer() {
  const p = state.player;
  if (!p) return;
  const now = p.now;
  const cover = $("#now-cover");
  if (now?.entry.track.thumbnail) {
    cover.style.backgroundImage = `url("${CSS.escape(now.entry.track.thumbnail)}")`;
    cover.textContent = "";
  } else {
    cover.style.backgroundImage = "";
    cover.textContent = "♪";
  }
  const title = $("#now-title");
  if (now) {
    const where = channelName(p.voice_channel);
    const listeners = p.listeners != null ? ` · ${p.listeners} Zuhörer` : "";
    $("#now-state").textContent =
      (now.paused ? "Pausiert" : now.ready ? "Läuft" : "Lädt …") + (where ? ` · 🔊 ${where}` : "") + listeners;
    title.textContent = now.entry.track.title;
    title.href = now.entry.track.url;
    $("#now-author").textContent = now.entry.track.author;
    $("#btn-playpause").textContent = now.paused ? "▶" : "⏸";
  } else {
    $("#now-state").textContent = p.voice_channel ? "Bereit" : "Nichts läuft";
    title.textContent = "Nichts läuft gerade";
    title.removeAttribute("href");
    $("#now-author").textContent = "Füge rechts etwas hinzu.";
    $("#btn-playpause").textContent = "⏯";
  }
  tickProgress();

  const labels = { off: "🔁 aus", one: "🔂 Track", all: "🔁 alle" };
  $("#btn-repeat").textContent = labels[p.repeat] || "🔁";
  const vol = $("#volume");
  if (document.activeElement !== vol) vol.value = p.volume;
  $("#volume-label").textContent = `${p.volume} %`;

  renderQueue();
}

/** Fortschritt zwischen zwei Abfragen lokal weiterzählen. */
function tickProgress() {
  const now = state.player?.now;
  if (!now) {
    $("#now-progress").style.width = "0%";
    $("#now-pos").textContent = "0:00";
    $("#now-len").textContent = "0:00";
    return;
  }
  let pos = now.position;
  if (!now.paused && now.ready) pos += (Date.now() - state.playerFetchedAt) / 1000;
  const len = now.entry.track.duration;
  if (len) pos = Math.min(pos, len);
  $("#now-pos").textContent = fmt(pos);
  $("#now-len").textContent = len ? fmt(len) : "Live";
  $("#now-progress").style.width = len ? `${(pos / len) * 100}%` : "100%";
}

async function action(name, body, button, okMsg) {
  await run(button, () => api("POST", `/player/${name}`, body ?? {}), okMsg);
  refreshPlayer();
}

$("#btn-playpause").addEventListener("click", (ev) =>
  action(state.player?.now?.paused ? "resume" : "pause", null, ev.currentTarget));
$("#btn-skip").addEventListener("click", (ev) => action("skip", null, ev.currentTarget, "Übersprungen"));
$("#btn-stop").addEventListener("click", (ev) => {
  if (confirm("Wiedergabe stoppen, Warteschlange leeren und Kanal verlassen?"))
    action("stop", null, ev.currentTarget, "Gestoppt");
});
$("#btn-shuffle").addEventListener("click", (ev) => action("shuffle", null, ev.currentTarget, "Gemischt"));
$("#btn-repeat").addEventListener("click", (ev) => {
  const next = { off: "one", one: "all", all: "off" }[state.player?.repeat || "off"];
  action("repeat", { mode: next }, ev.currentTarget);
});
$("#volume").addEventListener("change", (ev) => action("volume", { percent: Number(ev.target.value) }));
$("#volume").addEventListener("input", (ev) => ($("#volume-label").textContent = `${ev.target.value} %`));
$("#seek-form").addEventListener("submit", (ev) => {
  ev.preventDefault();
  action("seek", formData(ev.target), $("button", ev.target), "Gespult");
  ev.target.reset();
});

// Warteschlange -------------------------------------------------------------

async function queueEdit(body, { okMsg, undo = false } = {}) {
  try {
    const r = await api("POST", "/player/queue", body);
    const msg = typeof okMsg === "function" ? okMsg(r) : okMsg;
    if (undo && r.affected) {
      toast(msg || "Geändert", "ok", {
        label: "Rückgängig",
        fn: () => queueEdit({ op: "restore" }, { okMsg: "Wiederhergestellt" }),
      });
    } else if (msg) {
      toast(msg, "ok");
    }
    return r;
  } catch (e) {
    if (!(e instanceof Unauthorized)) toast(e.message, "error");
  } finally {
    refreshPlayer();
  }
}

const queueEntry = (id) => state.player?.queue.find((e) => e.id === id);
const queueTracks = (ids) => ids.map(queueEntry).filter(Boolean).map((e) => e.track);

/** Lokale Vorschau einer Verschiebung über IDs. */
function queueMovedLocally(ids, before, front) {
  const q = state.player.queue;
  const set = new Set(ids);
  const moved = q.filter((e) => set.has(e.id));
  const rest = q.filter((e) => !set.has(e.id));
  let at = front ? 0 : before == null ? rest.length : rest.findIndex((e) => e.id === before);
  if (at < 0) at = rest.length;
  rest.splice(at, 0, ...moved);
  state.player = { ...state.player, queue: rest };
  renderQueue();
}

function queueMove(ids, { before = null, front = false } = {}) {
  queueMovedLocally(ids, before, front);
  queueList.clearSelection();
  return queueEdit({ op: "move", ids, before, front });
}

function queueRemove(ids) {
  const first = queueEntry(ids[0]);
  queueList.clearSelection();
  return queueEdit({ op: "remove", ids }, {
    undo: true,
    okMsg: ids.length === 1 && first ? `„${first.track.title}“ entfernt` : `${ids.length} Titel entfernt`,
  });
}

async function queuePlayNow(id) {
  const r = await queueEdit({ op: "move", ids: [id], front: true });
  if (r) action("skip", null, null, "Spielt jetzt");
}

async function queueToPlaylist(name, ids) {
  const tracks = queueTracks(ids);
  if (!tracks.length) return;
  const r = await run(null, () => api("POST", `${plPath(name)}/append`, { tracks }),
    (r) => `${tracks.length} Titel zu „${name}“ hinzugefügt (jetzt ${r.total})`);
  if (r) {
    queueList.clearSelection();
    loadPlaylists();
  }
}

function newPlaylistFor(fn) {
  const n = prompt("Name der neuen Playlist:");
  if (n && n.trim()) fn(n.trim());
}

function queuePlaylistMenu(ids) {
  return [
    { heading: "In Playlist kopieren" },
    ...state.playlists.map((n) => ({ label: n, sub: true, fn: () => queueToPlaylist(n, ids) })),
    { label: "＋ Neue Playlist …", sub: true, fn: () => newPlaylistFor((n) => queueToPlaylist(n, ids)) },
  ];
}

const queueList = TrackList.create($("#queue"), {
  empty: "Die Warteschlange ist leer.",
  quick: [
    { act: "now", label: "▶", title: "Jetzt spielen" },
    { act: "remove", label: "✕", title: "Entfernen", danger: true },
  ],
  bulk: [
    { act: "next", label: "⏭ Als Nächstes" },
    { act: "end", label: "⤓ Ans Ende" },
    { act: "playlist", label: "In Playlist …" },
    { act: "remove", label: "✕ Entfernen", danger: true },
  ],
  onAction: (act, id) => {
    if (act === "now") queuePlayNow(id);
    if (act === "remove") queueRemove([id]);
  },
  onBulk: (act, ids, btn) => {
    if (act === "next") queueMove(ids, { front: true });
    if (act === "end") queueMove(ids);
    if (act === "playlist") TrackList.menuAt(btn, queuePlaylistMenu(ids));
    if (act === "remove") queueRemove(ids);
  },
  menu: (id) => {
    const q = state.player.queue;
    const i = q.findIndex((e) => e.id === id);
    const e = q[i];
    return [
      { label: "▶ Jetzt spielen", fn: () => queuePlayNow(id) },
      { label: "⏭ Als Nächstes", disabled: i === 0, fn: () => queueMove([id], { front: true }) },
      { label: "⤓ Ans Ende", disabled: i === q.length - 1, fn: () => queueMove([id]) },
      { label: "⏩ Bis hierhin überspringen", disabled: i === 0, fn: () => action("skipto", { index: i }, null, "Gesprungen") },
      "-",
      ...queuePlaylistMenu([id]),
      "-",
      { label: "🔗 Link kopieren", fn: () => copyLink(e.track.url) },
      { label: "✕ Entfernen", danger: true, fn: () => queueRemove([id]) },
    ];
  },
  onReorder: (ids, target, pos) => {
    let before = target;
    if (pos === "after") {
      const q = state.player.queue;
      const set = new Set(ids);
      const i = q.findIndex((e) => e.id === target);
      before = q.slice(i + 1).find((e) => !set.has(e.id))?.id ?? null;
    }
    queueMove(ids, { before });
    return ids[0];
  },
  dockTargets: () => [
    ...state.playlists.map((n) => ({ id: `pl:${n}`, label: `＋ ${n}` })),
    { id: "newpl", label: "＋ Neue Playlist" },
  ],
  onDropTarget: (id, ids) => {
    if (id.startsWith("pl:")) queueToPlaylist(id.slice(3), ids);
    if (id === "newpl") newPlaylistFor((n) => queueToPlaylist(n, ids));
  },
});

function renderQueue() {
  const p = state.player;
  $("#queue-meta").textContent = p.queue.length ? `· ${p.queue.length} Titel · ${fmt(p.queue_duration)}` : "";
  queueList.setItems(p.queue.map((e) => ({
    key: e.id,
    track: e.track,
    cls: e.autoplay ? "auto" : "",
    sub: e.autoplay ? ` · <span class="tag">Autoplaylist</span>` : e.guest ? " · Gast" : e.requester ? "" : " · Web",
  })));
  renderHistory();
}

/** „Zuletzt gespielt“ mit Knopf zum erneuten Einreihen. */
function renderHistory() {
  const h = state.player?.history ?? [];
  $("#history").innerHTML = h.length
    ? h.map((e, i) => `
        <li>
          ${e.track.thumbnail ? `<img loading="lazy" alt="" src="${esc(e.track.thumbnail)}">` : ""}
          <div class="meta">
            <a href="${esc(e.track.url)}" target="_blank" rel="noopener">${esc(e.track.title)}</a>
            <small>${esc(e.track.author)}${e.autoplay ? " · Autoplaylist" : e.guest ? " · Gast" : ""}</small>
          </div>
          <span class="dur">${fmt(e.track.duration)}</span>
          <div class="item-actions"><button class="btn small" data-again="${i}" title="Nochmal einreihen">↻ Nochmal</button></div>
        </li>`).join("")
    : `<li class="empty">Noch nichts gespielt.</li>`;
}

$("#history").addEventListener("click", (ev) => {
  const btn = ev.target.closest("button[data-again]");
  const entry = btn && state.player?.history?.[Number(btn.dataset.again)];
  if (entry) enqueueTracks([entry.track]);
});

$("#btn-queue-more").addEventListener("click", (ev) => {
  const q = state.player?.queue ?? [];
  const autos = q.filter((e) => e.autoplay).length;
  TrackList.menuAt(ev.currentTarget, [
    { label: "🔀 Mischen", disabled: q.length < 2, fn: () => action("shuffle", null, null, "Gemischt") },
    { label: "♊ Duplikate entfernen", disabled: q.length < 2, fn: () => queueEdit({ op: "dedupe" }, {
      undo: true,
      okMsg: (r) => r.affected ? `${r.affected} doppelte Titel entfernt` : "Keine Duplikate gefunden",
    }) },
    { label: `🔁 Autoplaylist-Titel entfernen${autos ? ` (${autos})` : ""}`, disabled: !autos, fn: () => queueEdit({ op: "remove_autoplay" }, {
      undo: true, okMsg: (r) => `${r.affected} Autoplaylist-Titel entfernt`,
    }) },
    "-",
    { label: "💾 Als Playlist speichern …", disabled: !q.length && !state.player?.now, fn: saveQueue },
    { label: "🗑 Leeren", danger: true, disabled: !q.length, fn: clearQueue },
  ]);
});

function saveQueue() {
  const name = prompt("Name der Playlist (aktueller Titel + Warteschlange):");
  if (name) run(null, () => api("POST", "/playlists/save-queue", { name }), (r) => `${r.saved} Titel gespeichert`).then(loadPlaylists);
}

async function clearQueue() {
  const r = await run(null, () => api("POST", "/player/clear"));
  refreshPlayer();
  if (r) toast("Warteschlange geleert – der aktuelle Titel läuft weiter", "ok", {
    label: "Rückgängig",
    fn: () => queueEdit({ op: "restore" }, { okMsg: "Wiederhergestellt" }),
  });
}

$("#play-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const body = formData(ev.target);
  if (!body.voice_channel) delete body.voice_channel;
  const r = await run($("button[type=submit]", ev.target), () => api("POST", "/player/play", body), (r) =>
    r.added === 1 ? (r.position === 0 ? "Spielt jetzt" : `Eingereiht an Position ${r.position}`) : `${r.added} Tracks eingereiht`);
  if (r) {
    $("input[name=query]", ev.target).value = "";
    if (r.skipped_too_long) toast(`${r.skipped_too_long} Track(s) zu lang – ausgelassen`);
  }
  refreshPlayer();
});

$("#btn-join").addEventListener("click", (ev) => {
  const voice_channel = $("#voice-select").value;
  if (!voice_channel) return toast("Bitte einen Sprachkanal auswählen", "error");
  run(ev.currentTarget, () => api("POST", "/player/join", { voice_channel }), (r) =>
    r.autoplaylist ? `Beigetreten – spiele Autoplaylist „${r.autoplaylist}“` : "Beigetreten").then(refreshPlayer);
});

// Drag & Drop: Links auf „Hinzufügen“ oder die Warteschlange ziehen.
const noLinkToast = () => toast("Das war kein YouTube- oder SoundCloud-Link", "error");

async function enqueueLinks(links) {
  const voice_channel = $("#voice-select").value || undefined;
  let added = 0;
  for (const query of links) {
    const r = await run(null, () => api("POST", "/player/play", { query, voice_channel }));
    if (r) added += r.added;
  }
  if (added) toast(added === 1 ? "1 Titel eingereiht" : `${added} Titel eingereiht`, "ok");
  refreshPlayer();
}
LeMusicDnD.attach($("#play-form"), enqueueLinks, noLinkToast);
LeMusicDnD.attach($("#queue-card"), enqueueLinks, noLinkToast);
