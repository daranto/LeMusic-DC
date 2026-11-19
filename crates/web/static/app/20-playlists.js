// app/20-playlists.js – Playlists und Verfügbarkeitsprüfung.
// Wird mit den anderen Dateien in app/ zu /app.js zusammengesetzt (assets.rs).

// ---------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------

/** Aktuell geöffnete Playlist (vom Server, mit Revision). */
const pl = {
  data: null,
  check: null,
  checkTimer: 0,
};

const plPath = (name) => `/playlists/${encodeURIComponent(name)}`;

async function addLinksToPlaylist(name, links) {
  let added = 0;
  for (const query of links) {
    const r = await run(null, () => api("POST", `${plPath(name)}/add`, { query }));
    if (r) added += r.added;
  }
  if (added) toast(`${added} Titel zu „${name}“ hinzugefügt`, "ok");
  await loadPlaylists();
  if (pl.data && sameName(pl.data.name, name)) showPlaylist(pl.data.name);
}

LeMusicDnD.attach($("#playlist-detail"), (links) => {
  if (!pl.data) return toast("Bitte zuerst eine Playlist auswählen – oder auf „Neue Playlist“ ziehen", "error");
  addLinksToPlaylist(pl.data.name, links);
}, noLinkToast);

LeMusicDnD.attach($("#playlist-create"), async (links) => {
  const form = $("#playlist-create");
  const name = form.elements.name.value.trim();
  if (!name) {
    form.elements.query.value = links.join(" ");
    form.elements.name.focus();
    return toast(links.length > 1 ? "Namen eingeben – mehrere Links: bitte auf eine bestehende Playlist ziehen" : "Jetzt noch einen Namen eingeben", "");
  }
  await addLinksToPlaylist(name, links);
  form.reset();
  showPlaylist(name);
}, noLinkToast);

async function loadPlaylists() {
  const lists = await run(null, () => api("GET", "/playlists"));
  if (!lists) return;
  state.playlists = lists.map((p) => p.name);
  const ul = $("#playlist-list");
  const current = pl.data?.name;
  ul.innerHTML = lists.length
    ? lists.map((p) => `
        <li data-name="${esc(p.name)}" data-drop-target="pl:${esc(p.name)}" class="${current && sameName(p.name, current) ? "active" : ""}">
          <div class="meta"><strong>${esc(p.name)}</strong><small>${p.track_count} Titel · ${fmt(p.duration_secs)}${state.autoplaylist && sameName(state.autoplaylist, p.name) ? " · 🔁 Autoplaylist" : ""}</small></div>
        </li>`).join("")
    : `<li class="empty">Noch keine Playlists.</li>`;
  // Links direkt auf eine Playlist in der Liste ziehen.
  $$("#playlist-list li[data-name]").forEach((li) =>
    LeMusicDnD.attach(li, (links) => addLinksToPlaylist(li.dataset.name, links), noLinkToast));
  if (current && !lists.some((p) => sameName(p.name, current))) closePlaylist();
}

function closePlaylist() {
  pl.data = null;
  stopCheckPolling();
  $("#playlist-detail").innerHTML = `<p class="muted">Playlist auswählen.</p>`;
}

$("#playlist-create").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const { name, query, soundcloud } = formData(ev.target);
  const btn = $("button[type=submit]", ev.target);
  const r = await run(btn, () => api("POST", `${plPath(name.trim())}/add`, { query, soundcloud }),
    (r) => `${r.added} Titel hinzugefügt – Playlist hat jetzt ${r.total}`);
  if (r) {
    ev.target.reset();
    await loadPlaylists();
    showPlaylist(name.trim());
  }
});

$("#playlist-import").addEventListener("change", async (ev) => {
  const file = ev.target.files[0];
  ev.target.value = "";
  if (!file) return;
  let data;
  try {
    data = JSON.parse(await file.text());
  } catch {
    return toast("Die Datei ist keine gültige JSON-Datei", "error");
  }
  if (!Array.isArray(data?.tracks)) return toast("In der Datei wurden keine Titel gefunden", "error");
  const name = prompt("Name der importierten Playlist:", data.name || file.name.replace(/\.json$/i, ""));
  if (!name) return;
  const r = await run(null, () => api("POST", "/playlists/import", { name, source: data.source || null, tracks: data.tracks }),
    (r) => `„${r.name}“ mit ${r.total} Titeln importiert`);
  if (r) {
    await loadPlaylists();
    showPlaylist(r.name);
  }
});

$("#playlist-list").addEventListener("click", (ev) => {
  const li = ev.target.closest("li[data-name]");
  if (li) showPlaylist(li.dataset.name);
});

const sameName = (a, b) => String(a).toLowerCase() === String(b).toLowerCase();

const plList = { list: null };

async function showPlaylist(name) {
  const p = await run(null, () => api("GET", plPath(name)));
  if (!p) return;
  if (!pl.data || !sameName(pl.data.name, p.name)) {
    stopCheckPolling();
    pl.check = null;
    renderPlaylistShell(p);
    pollCheck(p.name, true);
  }
  setPlaylist(p);
}

/** Übernimmt einen neuen Stand vom Server. */
function setPlaylist(p, { keepSelection = false } = {}) {
  pl.data = p;
  $$("#playlist-list li").forEach((li) => li.classList.toggle("active", !!li.dataset.name && sameName(li.dataset.name, p.name)));
  $("#pl-title").textContent = p.name;
  $("#pl-meta").textContent = `· ${p.tracks.length} Titel · ${fmt(p.duration)}`;
  const src = $("#pl-source");
  src.hidden = !p.source;
  if (p.source) {
    const a = $("#pl-source-link");
    a.href = p.source;
    a.textContent = p.source.replace(/^https?:\/\/(www\.)?/, "");
  }
  if (!keepSelection) plList.list.clearSelection();
  renderPlaylistItems();
  renderCheck();
}

function renderPlaylistItems() {
  const gone = new Map((pl.check?.gone || []).map((g) => [g.url, g.reason]));
  plList.list.setItems(pl.data.tracks.map((t, i) => ({
    key: i,
    track: t,
    cls: gone.has(t.url) ? "gone" : "",
    sub: gone.has(t.url) ? ` · <span class="tag bad">nicht verfügbar</span>` : "",
    title: gone.has(t.url) ? `Nicht mehr verfügbar: ${gone.get(t.url)}` : "",
  })));
}

function renderPlaylistShell(p) {
  const voice = state.guild?.voice_channels ?? [];
  const detail = $("#playlist-detail");
  detail.innerHTML = `
    <div class="card-head">
      <h2><span id="pl-title"></span> <span class="muted" id="pl-meta"></span></h2>
      <div class="row">
        <button class="btn small" id="pl-more" aria-haspopup="menu">⋯ Mehr</button>
      </div>
    </div>
    <p class="drop-note" id="pl-source" hidden>Quelle: <a id="pl-source-link" target="_blank" rel="noopener"></a>
      <button class="btn small" id="pl-sync" title="Neue Titel aus der Quelle übernehmen">↻ Aktualisieren</button></p>
    <form class="row" id="pl-play">
      <select name="voice_channel"><option value="">— aktueller Kanal des Bots —</option>
        ${voice.map((c) => `<option value="${esc(c.id)}">${esc(c.name)}</option>`).join("")}</select>
      <label class="check"><input type="checkbox" name="shuffle"> Zufällig</label>
      <button class="btn primary small" type="submit">▶ Abspielen</button>
    </form>
    <form class="row" id="pl-add">
      <input name="query" placeholder="Link oder Suche hinzufügen" required>
      <label class="check"><input type="checkbox" name="soundcloud"> SoundCloud</label>
      <button class="btn small" type="submit">Hinzufügen</button>
    </form>
    <div class="check-box" id="pl-check" hidden></div>
    <div id="pl-tracks"></div>`;

  plList.list = TrackList.create($("#pl-tracks"), {
    empty: "Leer – Links hierher ziehen oder oben hinzufügen.",
    dockHint: "Hierher ziehen (mit Shift: verschieben statt kopieren):",
    quick: [
      { act: "next", label: "⏭", title: "Als Nächstes spielen" },
      { act: "remove", label: "✕", title: "Entfernen", danger: true },
    ],
    bulk: [
      { act: "next", label: "⏭ Als Nächstes" },
      { act: "queue", label: "➕ Warteschlange" },
      { act: "top", label: "⤒ Nach oben" },
      { act: "bottom", label: "⤓ Nach unten" },
      { act: "copy", label: "Kopieren nach …" },
      { act: "move", label: "Verschieben nach …" },
      { act: "remove", label: "✕ Entfernen", danger: true },
    ],
    onAction: (act, i, btn) => plTrackAction(act, [i], btn),
    onBulk: (act, keys, btn) => plTrackAction(act, keys, btn),
    menu: (i) => plTrackMenu(i),
    onReorder: (keys, target, pos) => {
      const to = pos === "before" ? target : target + 1;
      plEdit([{ op: "move", indices: keys, to }], { quiet: true, optimistic: () => movedLocally(pl.data.tracks, keys, to) });
      // Fokus folgt dem verschobenen Titel.
      const before = keys.filter((k) => k < to).length;
      return to - before;
    },
    dockTargets: () => [
      { id: "queue:next", label: "⏭ Als Nächstes" },
      { id: "queue:end", label: "➕ Warteschlange" },
      ...state.playlists.filter((n) => !sameName(n, pl.data.name)).map((n) => ({ id: `pl:${n}`, label: `＋ ${n}` })),
    ],
    onDropTarget: (id, keys, { shift }) => {
      if (id === "queue:next") return plTrackAction("next", keys);
      if (id === "queue:end") return plTrackAction("queue", keys);
      if (id.startsWith("pl:")) {
        const target = id.slice(3);
        if (sameName(target, pl.data.name)) return;
        return copyToPlaylist(target, keys, shift);
      }
    },
  });

  $("#pl-more").addEventListener("click", (ev) => TrackList.menuAt(ev.currentTarget, playlistMenu()));
  $("#pl-sync").addEventListener("click", (ev) => syncPlaylist(ev.currentTarget));
  $("#pl-play").addEventListener("submit", (ev) => {
    ev.preventDefault();
    const body = formData(ev.target);
    if (!body.voice_channel) delete body.voice_channel;
    run($("button", ev.target), () => api("POST", `${plPath(pl.data.name)}/play`, body), (r) => `${r.added} Titel eingereiht`)
      .then(refreshPlayer);
  });
  $("#pl-add").addEventListener("submit", (ev) => {
    ev.preventDefault();
    const name = pl.data.name;
    run($("button", ev.target), () => api("POST", `${plPath(name)}/add`, formData(ev.target)), (r) => `${r.added} hinzugefügt`)
      .then((r) => {
        if (!r) return;
        ev.target.elements.query.value = "";
        showPlaylist(name);
        loadPlaylists();
      });
  });
}

/** Lokale Vorschau einer Verschiebung (sofortiges Feedback). */
function movedLocally(tracks, indices, to) {
  const set = new Set(indices);
  const moved = indices.slice().sort((a, b) => a - b).map((i) => tracks[i]);
  const rest = tracks.filter((_, i) => !set.has(i));
  const at = to - indices.filter((i) => i < to).length;
  rest.splice(at, 0, ...moved);
  return rest;
}

/**
 * Bearbeitung mit Revision. Bei Konflikt (409) wird neu geladen. Mit
 * `undo` erscheint ein „Rückgängig“-Knopf, der den alten Stand herstellt.
 */
async function plEdit(ops, { okMsg, undo = false, quiet = false, optimistic } = {}) {
  const before = pl.data;
  if (optimistic) {
    pl.data = { ...before, tracks: optimistic() };
    plList.list.clearSelection();
    renderPlaylistItems();
  }
  try {
    const r = await api("POST", `${plPath(before.name)}/edit`, { revision: before.revision, ops });
    setPlaylist(r);
    loadPlaylists();
    const msg = typeof okMsg === "function" ? okMsg(r) : okMsg;
    if (undo) {
      toast(msg || "Geändert", "ok", {
        label: "Rückgängig",
        fn: () => plEdit([{ op: "replace", tracks: before.tracks }], { okMsg: "Rückgängig gemacht", quiet: true }),
      });
    } else if (msg && !quiet) {
      toast(msg, "ok");
    } else if (msg) {
      toast(msg, "ok");
    }
    return r;
  } catch (e) {
    if (e instanceof Unauthorized) return;
    toast(e.message, "error");
    showPlaylist(before.name);
  }
}

function tracksAt(keys) {
  return keys.map((i) => pl.data.tracks[i]).filter(Boolean);
}

async function enqueueTracks(tracks, opts = {}) {
  const voice_channel = $("#pl-play select")?.value || $("#voice-select").value || undefined;
  const r = await run(null, () => api("POST", "/player/enqueue", { tracks, voice_channel, ...opts }), (r) =>
    opts.now ? "Spielt jetzt" : r.added === 1 ? (r.position === 0 ? "Spielt jetzt" : `Eingereiht an Position ${r.position}`) : `${r.added} Titel eingereiht`);
  refreshPlayer();
  return r;
}

async function copyToPlaylist(target, keys, move) {
  const name = pl.data.name;
  const r = await run(null, () => api("POST", `${plPath(target)}/append`, { tracks: tracksAt(keys) }));
  if (!r) return;
  if (move) {
    await plEdit([{ op: "remove", indices: keys }], { okMsg: `${keys.length} Titel nach „${target}“ verschoben`, undo: false });
  } else {
    toast(`${keys.length} Titel nach „${target}“ kopiert`, "ok");
    loadPlaylists();
  }
  if (!move && pl.data && sameName(pl.data.name, name)) plList.list.clearSelection();
}

function choosePlaylist(button, keys, move) {
  const others = state.playlists.filter((n) => !sameName(n, pl.data.name));
  TrackList.menuAt(button, [
    { heading: move ? "Verschieben nach" : "Kopieren nach" },
    ...others.map((n) => ({ label: n, sub: true, fn: () => copyToPlaylist(n, keys, move) })),
    { label: "＋ Neue Playlist …", sub: true, fn: () => {
      const n = prompt("Name der neuen Playlist:");
      if (n) copyToPlaylist(n.trim(), keys, move);
    } },
  ]);
}

function plTrackAction(act, keys, btn) {
  const tracks = tracksAt(keys);
  const n = pl.data.tracks.length;
  switch (act) {
    case "now": return enqueueTracks(tracks, { now: true });
    case "next": return enqueueTracks(tracks, { next: true });
    case "queue": return enqueueTracks(tracks);
    case "from": return run(null, () => api("POST", `${plPath(pl.data.name)}/play`, { start: keys[0], voice_channel: $("#pl-play select").value || undefined }),
      (r) => `${r.added} Titel ab Position ${keys[0] + 1} eingereiht`).then(refreshPlayer);
    case "top": return plEdit([{ op: "move", indices: keys, to: 0 }], { quiet: true, optimistic: () => movedLocally(pl.data.tracks, keys, 0) });
    case "bottom": return plEdit([{ op: "move", indices: keys, to: n }], { quiet: true, optimistic: () => movedLocally(pl.data.tracks, keys, n) });
    case "copy": return btn ? choosePlaylist(btn, keys, false) : null;
    case "move": return btn ? choosePlaylist(btn, keys, true) : null;
    case "link": return copyLink(tracks[0].url);
    case "remove":
      return plEdit([{ op: "remove", indices: keys }], {
        undo: true,
        okMsg: keys.length === 1 ? `„${tracks[0].title}“ entfernt` : `${keys.length} Titel entfernt`,
      });
  }
}

function plTrackMenu(i) {
  const n = pl.data.tracks.length;
  const others = state.playlists.filter((x) => !sameName(x, pl.data.name));
  return [
    { label: "▶ Jetzt spielen", fn: () => plTrackAction("now", [i]) },
    { label: "⏭ Als Nächstes", fn: () => plTrackAction("next", [i]) },
    { label: "➕ Ans Ende der Warteschlange", fn: () => plTrackAction("queue", [i]) },
    { label: "⤵ Playlist ab hier abspielen", fn: () => plTrackAction("from", [i]) },
    "-",
    { label: "⤒ Nach oben", disabled: i === 0, fn: () => plTrackAction("top", [i]) },
    { label: "⤓ Nach unten", disabled: i === n - 1, fn: () => plTrackAction("bottom", [i]) },
    ...(others.length ? ["-", { heading: "Kopieren nach" }, ...others.map((x) => ({ label: x, sub: true, fn: () => copyToPlaylist(x, [i], false) }))] : []),
    "-",
    { label: "🔗 Link kopieren", fn: () => plTrackAction("link", [i]) },
    { label: "✕ Entfernen", danger: true, fn: () => plTrackAction("remove", [i]) },
  ];
}

function playlistMenu() {
  const p = pl.data;
  const sort = (by, label) => ({
    label, sub: true,
    fn: () => plEdit([{ op: "sort", by }], { undo: true, okMsg: "Sortiert" }),
  });
  const running = pl.check?.running;
  return [
    { label: "✎ Umbenennen …", fn: renamePlaylist },
    { label: "⧉ Duplizieren …", fn: duplicatePlaylist },
    "-",
    { heading: "Sortieren" },
    sort("title", "nach Titel"),
    sort("author", "nach Interpret"),
    sort("duration", "nach Dauer"),
    sort("reverse", "Reihenfolge umkehren"),
    sort("shuffle", "dauerhaft mischen"),
    "-",
    { label: "♊ Duplikate entfernen", fn: () => plEdit([{ op: "dedupe" }], {
      undo: true,
      okMsg: (r) => r.affected ? `${r.affected} doppelte Titel entfernt` : "Keine Duplikate gefunden",
    }) },
    running
      ? { label: "■ Prüfung abbrechen", fn: cancelCheck }
      : { label: "🩺 Verfügbarkeit prüfen", disabled: !p.tracks.length, fn: startCheck },
    "-",
    p.source
      ? { label: "↻ Aus Quelle aktualisieren", fn: () => syncPlaylist(null) }
      : null,
    { label: p.source ? "Quelle ändern …" : "🔗 Quelle festlegen …", fn: setSource },
    p.source ? { label: "Quelle entfernen", fn: () => putSource(null) } : null,
    "-",
    { label: "⤓ Exportieren (JSON)", fn: exportPlaylist },
    { label: "🗑 Playlist löschen", danger: true, fn: deletePlaylist },
  ].filter(Boolean);
}

async function renamePlaylist() {
  const old = pl.data.name;
  const name = prompt("Neuer Name:", old);
  if (!name || name.trim() === old) return;
  const r = await run(null, () => api("POST", `${plPath(old)}/rename`, { name }), "Umbenannt");
  if (!r) return;
  if (state.autoplaylist && sameName(state.autoplaylist, old)) state.autoplaylist = r.name;
  pl.data = null;
  await loadPlaylists();
  showPlaylist(r.name);
}

async function duplicatePlaylist() {
  const name = prompt("Name der Kopie:", `${pl.data.name} (Kopie)`);
  if (!name) return;
  const r = await run(null, () => api("POST", `${plPath(pl.data.name)}/duplicate`, { name }), "Kopie angelegt");
  if (!r) return;
  await loadPlaylists();
  showPlaylist(r.name);
}

async function deletePlaylist() {
  const name = pl.data.name;
  const auto = state.autoplaylist && sameName(state.autoplaylist, name) ? "\nSie ist als Autoplaylist eingestellt!" : "";
  if (!confirm(`Playlist „${name}“ mit ${pl.data.tracks.length} Titeln endgültig löschen?${auto}`)) return;
  const r = await run(null, () => api("DELETE", plPath(name)), "Gelöscht");
  if (!r) return;
  closePlaylist();
  loadPlaylists();
}

async function exportPlaylist() {
  const data = await run(null, () => api("GET", `${plPath(pl.data.name)}/export`));
  if (!data) return;
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = `${data.name.replace(/[\\/:*?"<>|]+/g, "_")}.lemusic.json`;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 5000);
}

async function putSource(source) {
  const name = pl.data.name;
  const r = await run(null, () => api("PUT", `${plPath(name)}/source`, { source }), source ? "Quelle gespeichert" : "Quelle entfernt");
  if (r) showPlaylist(name);
}

function setSource() {
  const s = prompt("Link der YouTube-/SoundCloud-Playlist, aus der „Aktualisieren“ neue Titel holt:", pl.data.source || "");
  if (s != null) putSource(s.trim() || null);
}

async function syncPlaylist(button) {
  const r = await run(button, () => api("POST", `${plPath(pl.data.name)}/sync`), (r) =>
    r.added ? `${r.added} neue Titel übernommen` : "Keine neuen Titel in der Quelle");
  if (r) {
    setPlaylist(r);
    loadPlaylists();
  }
}

// Verfügbarkeitsprüfung -----------------------------------------------------

function stopCheckPolling() {
  clearTimeout(pl.checkTimer);
  pl.checkTimer = 0;
}

async function pollCheck(name, initial = false) {
  stopCheckPolling();
  let status;
  try {
    status = await api("GET", `${plPath(name)}/check`);
  } catch {
    return;
  }
  if (!pl.data || !sameName(pl.data.name, name)) return;
  const wasRunning = pl.check?.running;
  pl.check = status;
  renderCheck();
  renderPlaylistItems();
  if (status?.running) {
    pl.checkTimer = setTimeout(() => pollCheck(name), 2000);
  } else if (wasRunning && !initial && status) {
    toast(status.gone.length ? `Prüfung fertig: ${status.gone.length} Titel nicht mehr verfügbar` : "Prüfung fertig: alle Titel verfügbar",
      status.gone.length ? "error" : "ok");
  }
}

async function startCheck() {
  const name = pl.data.name;
  const r = await run(null, () => api("POST", `${plPath(name)}/check`), "Prüfung gestartet – läuft im Hintergrund weiter");
  if (r) pollCheck(name);
}

async function cancelCheck() {
  const name = pl.data.name;
  await run(null, () => api("DELETE", `${plPath(name)}/check`), "Prüfung wird abgebrochen");
  pollCheck(name);
}

function renderCheck() {
  const box = $("#pl-check");
  if (!box) return;
  const c = pl.check;
  if (!c) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  const goneHere = pl.data.tracks.filter((t) => c.gone.some((g) => g.url === t.url)).length;
  const pct = c.total ? Math.round((c.done / c.total) * 100) : 0;
  const unknown = c.unknown ? ` · ${c.unknown} nicht prüfbar (später erneut versuchen)` : "";
  if (c.running) {
    box.className = "check-box";
    box.innerHTML = `
      <div class="row"><strong>🩺 Prüfe Verfügbarkeit … ${c.done} / ${c.total}</strong>
        <span class="muted">${c.gone.length} nicht verfügbar${esc(unknown)}</span>
        <button class="btn small" id="pl-check-cancel">Abbrechen</button></div>
      <div class="progress"><div class="progress-fill" id="pl-check-bar"></div></div>`;
    $("#pl-check-bar").style.width = `${pct}%`;
    $("#pl-check-cancel").addEventListener("click", cancelCheck);
  } else {
    box.className = `check-box ${goneHere ? "bad" : "good"}`;
    box.innerHTML = goneHere
      ? `<div class="row"><strong>⚠ ${goneHere} Titel nicht mehr verfügbar</strong><span class="muted">(rot markiert${esc(unknown)})</span>
          <button class="btn small danger" id="pl-check-remove">Alle entfernen</button>
          <button class="btn small ghost" id="pl-check-close">Ausblenden</button></div>`
      : `<div class="row"><strong>✅ ${c.cancelled ? `Abgebrochen nach ${c.done} von ${c.total} Titeln – bis dahin alles verfügbar` : "Alle Titel verfügbar"}</strong>
          <span class="muted">${esc(unknown)}</span><button class="btn small ghost" id="pl-check-close">Ausblenden</button></div>`;
    $("#pl-check-close").addEventListener("click", () => {
      pl.check = null;
      renderCheck();
      renderPlaylistItems();
    });
    $("#pl-check-remove")?.addEventListener("click", () => {
      const gone = new Set(c.gone.map((g) => g.url));
      const indices = pl.data.tracks.map((t, i) => (gone.has(t.url) ? i : -1)).filter((i) => i >= 0);
      plEdit([{ op: "remove", indices }], { undo: true, okMsg: `${indices.length} nicht verfügbare Titel entfernt` });
    });
  }
}
