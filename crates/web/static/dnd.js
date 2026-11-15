"use strict";

// Drag & Drop von YouTube-/SoundCloud-Links – gemeinsam für Admin- und
// öffentliches Dashboard.
(() => {
  const HOSTS = new Set([
    "youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com", "youtu.be",
    "soundcloud.com", "www.soundcloud.com", "m.soundcloud.com", "on.soundcloud.com",
  ]);

  function isSupported(link) {
    try {
      const u = new URL(link);
      return (u.protocol === "https:" || u.protocol === "http:") && HOSTS.has(u.hostname.toLowerCase());
    } catch {
      return false;
    }
  }

  /** Alle unterstützten Links aus einem Text (Zeilen, Leerzeichen getrennt). */
  function linksFromText(text) {
    const found = String(text || "").match(/https?:\/\/[^\s<>"']+/g) || [];
    return [...new Set(found.map((l) => l.replace(/[).,;]+$/, "")))].filter(isSupported);
  }

  /** Links aus einem Drop: URL-Liste (Tabs, Adressleiste) oder Text. */
  function linksFromDrop(dt) {
    const uriList = (dt.getData("text/uri-list") || "")
      .split(/\r?\n/)
      .filter((l) => l && !l.startsWith("#"))
      .join("\n");
    return linksFromText(uriList || dt.getData("text/plain"));
  }

  /**
   * Macht `el` zur Ablagefläche. `onLinks(links)` bekommt die gefundenen
   * Links; `onInvalid()` wird gerufen, wenn nichts Passendes dabei war.
   */
  function attach(el, onLinks, onInvalid) {
    let depth = 0;
    const isLinkDrag = (ev) => {
      const types = [...(ev.dataTransfer?.types || [])];
      return types.includes("text/uri-list") || types.includes("text/plain");
    };
    el.addEventListener("dragenter", (ev) => {
      if (!isLinkDrag(ev)) return;
      ev.preventDefault();
      depth += 1;
      el.classList.add("drop-active");
    });
    el.addEventListener("dragover", (ev) => {
      if (!isLinkDrag(ev)) return;
      ev.preventDefault();
      ev.dataTransfer.dropEffect = "copy";
    });
    el.addEventListener("dragleave", () => {
      depth = Math.max(0, depth - 1);
      if (depth === 0) el.classList.remove("drop-active");
    });
    el.addEventListener("drop", (ev) => {
      if (!isLinkDrag(ev)) return;
      ev.preventDefault();
      ev.stopPropagation();
      depth = 0;
      el.classList.remove("drop-active");
      const links = linksFromDrop(ev.dataTransfer);
      if (links.length) onLinks(links);
      else if (onInvalid) onInvalid();
    });
  }

  // Versehentlich daneben fallen gelassene Links nicht als Seite öffnen.
  window.addEventListener("dragover", (ev) => ev.preventDefault());
  window.addEventListener("drop", (ev) => ev.preventDefault());

  window.LeMusicDnD = { attach, linksFromText, isSupported };
})();
