"use strict";

// Titelliste für Warteschlange und Playlists: Drag & Drop zum Sortieren
// (Maus, Touch, Tastatur), Mehrfachauswahl, Filter, Kontextmenü.
//
// Interne Verschiebungen laufen über Pointer-Events statt HTML5-Drag&Drop –
// so funktioniert es auch auf Touch-Geräten und kommt dem Hineinziehen von
// YouTube-/SoundCloud-Links (dnd.js) nicht in die Quere.
(() => {
  const esc = (s) =>
    String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

  function fmt(secs) {
    if (secs == null) return "Live";
    secs = Math.max(0, Math.floor(secs));
    const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), s = secs % 60;
    const ss = String(s).padStart(2, "0");
    return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
  }

  // -------------------------------------------------------------------------
  // Kontextmenü
  // -------------------------------------------------------------------------

  let openMenu = null;

  function closeMenu() {
    if (openMenu) {
      openMenu.remove();
      openMenu = null;
    }
  }

  /**
   * Zeigt ein Menü an (x, y). Einträge: `{ label, fn, danger, disabled }`,
   * `"-"` (Trenner) oder `{ heading }`.
   */
  function showMenu(x, y, items) {
    closeMenu();
    const menu = document.createElement("div");
    menu.className = "menu";
    menu.setAttribute("role", "menu");
    for (const it of items) {
      if (it === "-") {
        menu.append(Object.assign(document.createElement("hr"), {}));
      } else if (it.heading) {
        const h = document.createElement("div");
        h.className = "menu-heading";
        h.textContent = it.heading;
        menu.append(h);
      } else {
        const b = document.createElement("button");
        b.type = "button";
        b.setAttribute("role", "menuitem");
        b.textContent = it.label;
        if (it.danger) b.classList.add("danger");
        if (it.sub) b.classList.add("sub");
        b.disabled = !!it.disabled;
        b.addEventListener("click", (ev) => {
          ev.stopPropagation();
          closeMenu();
          it.fn(ev);
        });
        menu.append(b);
      }
    }
    document.body.append(menu);
    const r = menu.getBoundingClientRect();
    const left = Math.max(8, Math.min(x, innerWidth - r.width - 8));
    const top = y + r.height > innerHeight - 8 ? Math.max(8, y - r.height) : y;
    menu.style.left = `${left}px`;
    menu.style.top = `${top}px`;
    openMenu = menu;
    menu.querySelector("button:not(:disabled)")?.focus({ preventScroll: true });
  }

  document.addEventListener("pointerdown", (ev) => {
    if (openMenu && !openMenu.contains(ev.target)) closeMenu();
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") closeMenu();
  });
  addEventListener("scroll", closeMenu, { passive: true });
  addEventListener("resize", closeMenu);

  /** Menü unter einem Button öffnen. */
  function menuAt(button, items) {
    const r = button.getBoundingClientRect();
    showMenu(r.left, r.bottom + 4, items);
  }

  // -------------------------------------------------------------------------
  // Liste
  // -------------------------------------------------------------------------

  /**
   * @param root   Container (wird gefüllt)
   * @param opts {
   *   quick: [{ act, label, title, danger }]      Schnellknöpfe pro Zeile
   *   bulk:  [{ act, label, danger }]             Aktionen für die Auswahl
   *   onAction(act, key, button)                  Schnellknopf / Menü
   *   menu(key) -> items                          Kontextmenü einer Zeile
   *   onBulk(act, keys, button)
   *   onReorder(keys, targetKey, "before"|"after") -> neuer Fokus-Schlüssel
   *   dockTargets(keys) -> [{ id, label }]        Ziele während des Ziehens
   *   onDropTarget(id, keys, { shift })           Auf Ziel fallen gelassen
 *   dockHint: "Text vor den Zielen"
   *   empty: "Text bei leerer Liste"
   * }
   * Items: `{ key, track, sub, cls, title }`
   */
  function create(root, opts) {
    root.classList.add("tracklist");
    root.innerHTML = `
      <div class="tl-toolbar">
        <label class="check tl-all" title="Alle (sichtbaren) auswählen"><input type="checkbox"></label>
        <input class="tl-filter" type="search" placeholder="Filtern nach Titel oder Interpret" aria-label="Filtern">
        <span class="muted tl-count"></span>
      </div>
      <div class="tl-bulk" hidden>
        <strong class="tl-selcount"></strong>
        <div class="row tl-bulk-actions">
          ${(opts.bulk || []).map((b) => `<button type="button" class="btn small ${b.danger ? "danger" : ""}" data-bulk="${esc(b.act)}">${esc(b.label)}</button>`).join("")}
          <button type="button" class="btn small ghost" data-bulk="__clear">Auswahl aufheben</button>
        </div>
      </div>
      <ol class="queue tl-list"></ol>`;
    const listEl = root.querySelector(".tl-list");
    const filterEl = root.querySelector(".tl-filter");
    const allEl = root.querySelector(".tl-all input");
    const bulkEl = root.querySelector(".tl-bulk");

    const self = {
      items: [],
      selected: new Set(),
      signature: "",
      dragging: false,
      pending: null,
      focusKey: null,
      anchor: null,
      filter: "",
    };

    const keyStr = (k) => String(k);
    const findItem = (k) => self.items.find((it) => keyStr(it.key) === keyStr(k));
    const orderedSelection = () => self.items.filter((it) => self.selected.has(keyStr(it.key))).map((it) => it.key);
    const visibleRows = () => [...listEl.querySelectorAll("li[data-key]:not([hidden])")];

    function matches(it) {
      if (!self.filter) return true;
      const hay = `${it.track.title} ${it.track.author}`.toLowerCase();
      return self.filter.split(/\s+/).every((w) => hay.includes(w));
    }

    function rowHtml(it, i) {
      const t = it.track;
      const k = esc(keyStr(it.key));
      const sel = self.selected.has(keyStr(it.key));
      return `
        <li data-key="${k}" class="${sel ? "selected " : ""}${esc(it.cls || "")}" ${matches(it) ? "" : "hidden"} ${it.title ? `title="${esc(it.title)}"` : ""}>
          <button type="button" class="handle" aria-label="Ziehen zum Verschieben (Pfeiltasten verschieben)" title="Ziehen zum Verschieben · Pfeiltasten ↑/↓">⠿</button>
          <input type="checkbox" class="sel" aria-label="Auswählen" ${sel ? "checked" : ""}>
          <span class="pos">${i + 1}</span>
          ${t.thumbnail ? `<img loading="lazy" alt="" draggable="false" src="${esc(t.thumbnail)}">` : `<span class="noimg">♪</span>`}
          <div class="meta">
            <a href="${esc(t.url)}" target="_blank" rel="noopener" draggable="false">${esc(t.title)}</a>
            <small>${esc(t.author)}${it.sub || ""}</small>
          </div>
          <span class="dur">${fmt(t.duration)}</span>
          <div class="item-actions">
            ${(opts.quick || []).map((q) => `<button type="button" class="btn ${q.danger ? "danger" : ""}" data-act="${esc(q.act)}" title="${esc(q.title)}">${esc(q.label)}</button>`).join("")}
            <button type="button" class="btn" data-act="__menu" title="Weitere Aktionen" aria-haspopup="menu">⋯</button>
          </div>
        </li>`;
    }

    function updateMeta() {
      for (const k of [...self.selected]) if (!findItem(k)) self.selected.delete(k);
      const n = self.selected.size;
      bulkEl.hidden = n === 0;
      root.querySelector(".tl-selcount").textContent = n === 1 ? "1 ausgewählt" : `${n} ausgewählt`;
      const visible = self.items.filter(matches);
      const count = root.querySelector(".tl-count");
      count.textContent = self.filter ? `${visible.length} von ${self.items.length}` : "";
      const visSel = visible.filter((it) => self.selected.has(keyStr(it.key))).length;
      allEl.checked = visible.length > 0 && visSel === visible.length;
      allEl.indeterminate = visSel > 0 && visSel < visible.length;
      root.querySelector(".tl-toolbar").hidden = self.items.length < 2 && !self.filter;
    }

    function render() {
      listEl.innerHTML = self.items.length
        ? self.items.map(rowHtml).join("")
        : `<li class="empty">${esc(opts.empty || "Leer.")}</li>`;
      updateMeta();
      applyFocus();
    }

    /** Fokus nach dem Neuzeichnen wieder auf den verschobenen Griff. */
    function applyFocus() {
      if (self.focusKey == null) return;
      const row = listEl.querySelector(`li[data-key="${CSS.escape(keyStr(self.focusKey))}"]`);
      const handle = row?.querySelector(".handle");
      if (handle && document.activeElement !== handle) handle.focus({ preventScroll: false });
      if (handle) self.focusKey = null;
    }

    /** Neue Einträge setzen. Neu gezeichnet wird nur bei Änderungen. */
    function setItems(items, { force = false } = {}) {
      if (self.dragging) {
        self.pending = items;
        return;
      }
      const sig = items.map((it) => `${keyStr(it.key)}|${it.track.url}|${it.cls || ""}|${it.sub || ""}`).join("\n");
      self.items = items;
      if (!force && sig === self.signature) {
        updateMeta();
        return;
      }
      self.signature = sig;
      render();
    }

    function setSelected(key, on) {
      const k = keyStr(key);
      if (on) self.selected.add(k);
      else self.selected.delete(k);
      const row = listEl.querySelector(`li[data-key="${CSS.escape(k)}"]`);
      if (row) {
        row.classList.toggle("selected", on);
        const cb = row.querySelector(".sel");
        if (cb) cb.checked = on;
      }
    }

    function clearSelection() {
      for (const k of [...self.selected]) setSelected(k, false);
      self.anchor = null;
      updateMeta();
    }

    // Filter & Auswahl
    filterEl.addEventListener("input", () => {
      self.filter = filterEl.value.trim().toLowerCase();
      for (const row of listEl.querySelectorAll("li[data-key]")) {
        const it = findItem(row.dataset.key);
        row.hidden = it ? !matches(it) : false;
      }
      updateMeta();
    });
    allEl.addEventListener("change", () => {
      const on = allEl.checked;
      for (const it of self.items.filter(matches)) setSelected(it.key, on);
      updateMeta();
    });

    listEl.addEventListener("click", (ev) => {
      const row = ev.target.closest("li[data-key]");
      if (!row) return;
      const key = row.dataset.key;
      const item = findItem(key);
      if (ev.target.classList.contains("sel")) {
        const on = ev.target.checked;
        if (ev.shiftKey && self.anchor != null) {
          const rows = visibleRows().map((r) => r.dataset.key);
          const a = rows.indexOf(keyStr(self.anchor)), b = rows.indexOf(key);
          if (a >= 0 && b >= 0) {
            const [lo, hi] = a < b ? [a, b] : [b, a];
            rows.slice(lo, hi + 1).forEach((k) => setSelected(k, on));
          }
        } else {
          setSelected(key, on);
        }
        self.anchor = key;
        updateMeta();
        return;
      }
      const btn = ev.target.closest("button[data-act]");
      if (btn && item) {
        if (btn.dataset.act === "__menu") menuAt(btn, opts.menu(item.key));
        else opts.onAction(btn.dataset.act, item.key, btn);
      }
    });

    listEl.addEventListener("contextmenu", (ev) => {
      const row = ev.target.closest("li[data-key]");
      if (!row || ev.target.closest("a")) return;
      const item = findItem(row.dataset.key);
      if (!item) return;
      ev.preventDefault();
      showMenu(ev.clientX, ev.clientY, opts.menu(item.key));
    });

    bulkEl.addEventListener("click", (ev) => {
      const btn = ev.target.closest("button[data-bulk]");
      if (!btn) return;
      if (btn.dataset.bulk === "__clear") return clearSelection();
      opts.onBulk(btn.dataset.bulk, orderedSelection(), btn);
    });

    // Tastatur: Pfeiltasten auf dem Griff verschieben um eine Position.
    listEl.addEventListener("keydown", (ev) => {
      if (!ev.target.classList.contains("handle")) return;
      if (ev.key !== "ArrowUp" && ev.key !== "ArrowDown") return;
      ev.preventDefault();
      const row = ev.target.closest("li[data-key]");
      const rows = visibleRows();
      const i = rows.indexOf(row);
      const up = ev.key === "ArrowUp";
      const neighbor = rows[up ? i - 1 : i + 1];
      if (!neighbor) return;
      self.focusKey = row.dataset.key;
      const focus = opts.onReorder([findItem(row.dataset.key).key], findItem(neighbor.dataset.key).key, up ? "before" : "after");
      if (focus != null) self.focusKey = focus;
      applyFocus();
    });

    // ---------------------------------------------------------------------
    // Ziehen
    // ---------------------------------------------------------------------

    let drag = null;

    function startDrag(ev, row) {
      const key = row.dataset.key;
      const keys = self.selected.has(key) ? orderedSelection() : [findItem(key).key];
      const ghost = document.createElement("div");
      ghost.className = "drag-ghost";
      ghost.textContent = keys.length === 1 ? findItem(keys[0]).track.title : `${keys.length} Titel`;
      document.body.append(ghost);
      const dock = document.createElement("div");
      dock.className = "drag-dock";
      const targets = opts.dockTargets ? opts.dockTargets(keys) : [];
      dock.innerHTML = targets.length
        ? `<span class="muted">${esc(opts.dockHint || "Hierher ziehen:")}</span>${targets.map((t) => `<span class="dock-target" data-drop-target="${esc(t.id)}">${esc(t.label)}</span>`).join("")}`
        : "";
      if (targets.length) document.body.append(dock);
      const keySet = new Set(keys.map(keyStr));
      listEl.querySelectorAll("li[data-key]").forEach((r) => r.classList.toggle("dragging", keySet.has(r.dataset.key)));
      document.body.classList.add("is-dragging");
      self.dragging = true;
      drag = { keys, keySet, ghost, dock, over: null, target: null, x: ev.clientX, y: ev.clientY, raf: 0 };
      moveDrag(ev);
      autoScroll();
    }

    function clearIndicators() {
      listEl.querySelectorAll(".drop-before, .drop-after").forEach((r) => r.classList.remove("drop-before", "drop-after"));
      document.querySelectorAll(".drop-hover").forEach((t) => t.classList.remove("drop-hover"));
    }

    function moveDrag(ev) {
      drag.x = ev.clientX;
      drag.y = ev.clientY;
      drag.shift = ev.shiftKey;
      drag.ghost.style.transform = `translate(${ev.clientX + 14}px, ${ev.clientY + 10}px)`;
      clearIndicators();
      drag.over = null;
      drag.target = null;
      const el = document.elementFromPoint(ev.clientX, ev.clientY);
      if (!el) return;
      const target = el.closest("[data-drop-target]");
      if (target) {
        target.classList.add("drop-hover");
        drag.target = target.dataset.dropTarget;
        return;
      }
      const row = el.closest("li[data-key]");
      if (row && listEl.contains(row) && !drag.keySet.has(row.dataset.key)) {
        const r = row.getBoundingClientRect();
        const pos = ev.clientY < r.top + r.height / 2 ? "before" : "after";
        row.classList.add(pos === "before" ? "drop-before" : "drop-after");
        drag.over = { key: findItem(row.dataset.key).key, pos };
      }
    }

    function autoScroll() {
      if (!drag) return;
      const edge = 70;
      let dy = 0;
      if (drag.y < edge) dy = -Math.ceil((edge - drag.y) / 4);
      else if (drag.y > innerHeight - edge) dy = Math.ceil((drag.y - (innerHeight - edge)) / 4);
      if (dy) {
        scrollBy(0, dy);
        moveDrag({ clientX: drag.x, clientY: drag.y, shiftKey: drag.shift });
      }
      drag.raf = requestAnimationFrame(autoScroll);
    }

    function endDrag(commit) {
      if (!drag) return;
      const d = drag;
      drag = null;
      cancelAnimationFrame(d.raf);
      clearIndicators();
      d.ghost.remove();
      d.dock.remove();
      document.body.classList.remove("is-dragging");
      listEl.querySelectorAll(".dragging").forEach((r) => r.classList.remove("dragging"));
      self.dragging = false;
      if (self.pending) {
        const p = self.pending;
        self.pending = null;
        setItems(p);
      }
      if (!commit) return;
      if (d.target && opts.onDropTarget) opts.onDropTarget(d.target, d.keys, { shift: d.shift });
      else if (d.over) opts.onReorder(d.keys, d.over.key, d.over.pos);
    }

    let press = null;
    listEl.addEventListener("pointerdown", (ev) => {
      const handle = ev.target.closest(".handle");
      if (!handle || ev.button !== 0) return;
      ev.preventDefault();
      try {
        handle.setPointerCapture(ev.pointerId);
      } catch {
        /* z. B. synthetische Events */
      }
      press = { id: ev.pointerId, x: ev.clientX, y: ev.clientY, row: handle.closest("li[data-key]"), handle };
    });
    listEl.addEventListener("pointermove", (ev) => {
      if (!press || ev.pointerId !== press.id) return;
      if (!drag) {
        if (Math.hypot(ev.clientX - press.x, ev.clientY - press.y) < 4) return;
        startDrag(ev, press.row);
      }
      moveDrag(ev);
    });
    const finish = (commit) => (ev) => {
      if (!press || ev.pointerId !== press.id) return;
      const wasDrag = !!drag;
      press = null;
      if (wasDrag) endDrag(commit);
    };
    listEl.addEventListener("pointerup", finish(true));
    listEl.addEventListener("pointercancel", finish(false));
    document.addEventListener("keydown", (ev) => {
      if (ev.key === "Escape" && drag) {
        press = null;
        endDrag(false);
      }
    });

    return {
      setItems,
      clearSelection,
      selectedKeys: orderedSelection,
      get dragging() { return self.dragging; },
      get items() { return self.items; },
    };
  }

  window.TrackList = { create, showMenu, menuAt, closeMenu, fmt };
})();
