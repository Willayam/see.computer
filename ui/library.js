(() => {
  const tauri = window.__TAURI__;
  let items = [];
  let loadVersion = 0;
  const $ = (sel, root = document) => root.querySelector(sel);
  const params = new URLSearchParams(location.search);
  const mediaUrl = (path) => esc(tauri.core.convertFileSrc(path));

  const GLYPH = {
    dictation: '<svg viewBox="0 0 16 16"><path d="M2 8v1M5 5v6M8 3v10M11 5v6M14 7v2"/></svg>',
    take: '<svg viewBox="0 0 16 16"><rect x="1.5" y="3.5" width="9" height="9" rx="2"/><path d="M10.5 7 14.5 5v6l-4-2"/></svg>',
    meeting: '<svg viewBox="0 0 16 16"><circle cx="5.5" cy="5.5" r="2.5"/><circle cx="11" cy="6" r="2"/><path d="M1.5 13.5c.4-2.6 2-4 4-4s3.6 1.4 4 4M9.5 9.8c1.9 0 3.6 1 4.5 3.7"/></svg>',
  };
  const ICON = {
    copy: '<svg viewBox="0 0 16 16"><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 5.5v-2a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2"/></svg>',
    check: '<svg viewBox="0 0 16 16"><path d="m3 8.5 3 3 7-7"/></svg>',
    finder: '<svg viewBox="0 0 16 16"><path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v6a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 12z"/></svg>',
    link: '<svg viewBox="0 0 16 16"><path d="M6.5 9.5a3 3 0 0 0 4.2 0l2-2a3 3 0 0 0-4.2-4.2l-1 1"/><path d="M9.5 6.5a3 3 0 0 0-4.2 0l-2 2a3 3 0 0 0 4.2 4.2l1-1"/></svg>',
    rec: '<svg viewBox="0 0 16 16"><circle cx="8" cy="8" r="3" fill="currentColor" stroke="none"/><circle cx="8" cy="8" r="6"/></svg>',
    spin: '<svg viewBox="0 0 16 16"><path d="M8 2a6 6 0 1 1-4.2 1.7"/></svg>',
    warn: '<svg viewBox="0 0 16 16"><path d="M8 2.5 14 13H2z"/><path d="M8 6.5v3M8 11.3v.2"/></svg>',
  };
  const NOUN = { dictation: "Dictations", take: "Screen take", meeting: "Meeting" };

  let kind = "all";
  let query = "";
  let currentId = null;

  const fmt = {
    day(ms) {
      const d = new Date(ms), now = new Date();
      const sameDay = (a, b) => a.toDateString() === b.toDateString();
      if (sameDay(d, now)) return "Today";
      const y = new Date(now); y.setDate(now.getDate() - 1);
      if (sameDay(d, y)) return "Yesterday";
      return d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
    },
    clock(ms) { return new Date(ms).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" }); },
    clockS(ms) { return new Date(ms).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit", second: "2-digit" }); },
    long(ms) { return new Date(ms).toLocaleDateString("en-US", { weekday: "long", month: "long", day: "numeric", year: "numeric" }); },
    dur(ms) {
      if (ms == null) return "";
      const s = Math.round(ms / 1000), h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
      return h ? `${h}:${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}` : `${m}:${String(r).padStart(2, "0")}`;
    },
    offset(ms) {
      const s = Math.floor(ms / 1000), h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
      return (h ? `${h}:` : "") + `${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}`;
    },
  };

  const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

  function visible() {
    const q = query.trim().toLowerCase();
    return items.filter((i) => (kind === "all" || i.kind === kind) &&
      (!q || i.title.toLowerCase().includes(q) || i.lines.some((l) => l.text.toLowerCase().includes(q))));
  }

  function rowTitle(i) {
    if (i.kind === "dictation") return i.title;
    if (i.kind === "take") return i.title;
    return i.title;
  }
  function rowSub(i) {
    if (i.kind === "dictation") return i.lines[0]?.text ?? "";
    if (i.kind === "take") return [fmt.dur(i.durationMs), `${i.media.filter((m) => m.kind !== "clip").length} captures`].filter(Boolean).join(" · ");
    if (i.state === "recording") return i.lines[0]?.text || "Recording…";
    if (i.state === "transcribing") return "Transcribing…";
    if (i.state === "failed") return i.error || "Transcription failed";
    return i.lines[0]?.text ?? "";
  }

  function renderList() {
    const list = $("#list");
    const rows = visible();
    list.innerHTML = "";
    let lastDay = null;
    for (const i of rows) {
      const day = fmt.day(i.at);
      if (day !== lastDay) {
        const h = document.createElement("div");
        h.className = "day"; h.textContent = day;
        list.appendChild(h);
        lastDay = day;
      }
      const el = $("#t-row").content.firstElementChild.cloneNode(true);
      el.dataset.id = i.id;
      el.classList.toggle("is-on", i.id === currentId);
      el.classList.toggle("is-live", i.state === "recording");
      el.classList.toggle("is-busy", i.state === "transcribing");
      $(".row-glyph", el).innerHTML = GLYPH[i.kind];
      $(".row-title", el).textContent = rowTitle(i);
      $(".row-sub", el).textContent = rowSub(i);
      $(".row-time", el).textContent = i.state === "recording" ? "live" : i.kind === "dictation" ? `${fmt.clock(i.lines[0].startMs)}–${fmt.clock(i.lines.at(-1).startMs)}` : fmt.clock(i.at);
      el.addEventListener("click", () => select(i.id));
      list.appendChild(el);
    }
    if (!rows.length) list.innerHTML = '<div class="empty">Nothing matches.</div>';
    $("#count").textContent = `${rows.length} of ${items.length}`;
  }

  function select(id) {
    if (!items.some((item) => item.id === id)) return;
    currentId = id;
    renderList();
    renderDetail();
    $("#detail").scrollTop = 0;
  }

  function kicker(i) {
    const state = i.state === "recording" ? '<span class="state rec">Recording</span>'
      : i.state === "transcribing" ? '<span class="state busy">Transcribing</span>'
      : i.state === "failed" ? '<span class="state bad">Failed</span>' : "";
    return `<div class="kicker">${GLYPH[i.kind]}<span>${NOUN[i.kind]}</span>${state ? '<span class="sep">·</span>' + state : ""}</div>`;
  }

  function title(i) {
    if (i.kind === "dictation") return `${fmt.long(i.at).split(",")[0]}'s dictations`;
    if (i.kind === "take") {
      const full = i.lines.map((l) => l.text).join(" ").trim();
      if (!full) return "Screen take";
      const sentence = full.split(/(?<=[.?!])\s/)[0];
      return sentence.length > 64 ? sentence.split(/\s+/).slice(0, 9).join(" ") + "…" : sentence;
    }
    return i.title;
  }

  function when(i) {
    const start = fmt.long(i.at);
    if (i.kind === "dictation") return `${start} · ${i.lines.length} entries from ${fmt.clock(i.lines[0].startMs)} to ${fmt.clock(i.lines.at(-1).startMs)}`;
    if (i.state === "recording") return `${start} · started ${fmt.clock(i.at)}`;
    if (i.durationMs == null) return `${start} · ${fmt.clock(i.at)}`;
    return `${start} · ${fmt.clock(i.at)} to ${fmt.clock(i.at + i.durationMs)}`;
  }

  function meta(i) {
    const parts = [];
    if (i.kind === "meeting") {
      const you = i.lines.filter((l) => l.speaker === "you").length, them = i.lines.length - you;
      if (i.state !== "transcribing") parts.push(`<span><span class="you">You</span> ${you} · <span class="them">Them</span> ${them}</span>`);
      if (i.state === "recording") parts.push(`<span class="live">${fmt.dur(Date.now() - i.at)} so far</span>`);
      else if (i.durationMs != null) parts.push(`<span>${fmt.dur(i.durationMs)}</span>`);
      if (i.source) parts.push(`<span>${esc(i.source)}</span>`);
    } else if (i.kind === "take") {
      parts.push(`<span>${fmt.dur(i.durationMs)}</span>`);
      const shots = i.media.filter((m) => m.kind === "shot").length, clips = i.media.filter((m) => m.kind === "clip").length, frames = i.media.filter((m) => m.kind === "frame").length;
      if (clips) parts.push(`<span>${clips} clip${clips > 1 ? "s" : ""}</span>`);
      if (shots) parts.push(`<span>${shots} shot${shots > 1 ? "s" : ""}</span>`);
      if (frames) parts.push(`<span>${frames} frames</span>`);
      if (i.share?.url) parts.push(`<span>Shared</span>`);
    } else {
      parts.push(`<span>${i.lines.length} entries</span>`);
      const words = i.lines.reduce((n, l) => n + l.text.split(/\s+/).length, 0);
      parts.push(`<span>${words.toLocaleString()} words</span>`);
    }
    return parts.join('<span class="sep">·</span>');
  }

  function actions(i) {
    const a = [];
    a.push(`<button class="act is-primary" data-act="copy">${ICON.copy}<span>Copy</span></button>`);
    if (i.kind === "take" && i.share?.url) a.push(`<button class="act" data-act="link">${ICON.link}<span>Share link</span></button>`);
    a.push(`<button class="act" data-act="finder" title="${esc(i.files.markdown || i.files.folder)}">${ICON.finder}<span>Reveal</span></button>`);
    return a.join("");
  }

  function audio(i) {
    const tracks = i.kind === "meeting" && i.state !== "recording" ? i.media.filter((m) => m.kind === "audio") : [];
    if (!tracks.length) return "";
    return `<div class="audio">${tracks.map((m) =>
      `<div class="audio-track ${m.speaker}"><span>${m.speaker}.wav</span><audio controls preload="none" src="${mediaUrl(m.path)}"></audio></div>`).join("")}</div>`;
  }

  function notice(i) {
    if (i.state === "recording") return `<div class="notice rec">${ICON.rec}<div><strong>Recording this call.</strong> New lines appear during the call. The transcript and audio appear here when processing finishes.</div></div>`;
    if (i.state === "transcribing") return `<div class="notice">${ICON.spin}<div><strong>Transcribing ${fmt.dur(i.durationMs)} of audio.</strong> The transcript will appear here when it is ready.</div></div>`;
    if (i.state === "failed") return `<div class="notice bad">${ICON.warn}<div><strong>Transcription failed.</strong> ${esc(i.error || "The audio is still in the folder.")}</div></div>`;
    return "";
  }

  function lineEl(i, l, idx) {
    const cls = ["line", `is-${i.kind}`];
    if (l.speaker) cls.push(`is-${l.speaker}`);
    const time = i.kind === "dictation" ? fmt.clock(l.startMs) : fmt.offset(l.startMs);
    const speaker = l.speaker ? `<span class="speaker">${l.speaker === "you" ? "You" : "Them"}</span>` : "";
    return `<div class="${cls.join(" ")}" data-idx="${idx}">
      <span class="line-time" title="${i.kind === "dictation" ? fmt.clockS(l.startMs) : ""}">${time}</span>
      <div class="line-body">${speaker}${esc(l.text)}</div>
      <button class="line-copy" data-act="copy-line" data-idx="${idx}" title="Copy this line">${ICON.copy}</button>
    </div>`;
  }

  function mediaEl(m) {
    const cap = m.kind === "clip" ? `Clip at ${fmt.offset(m.atMs)} · ${fmt.dur(m.durationMs)}` : `${m.kind === "shot" ? "Screenshot" : "Frame"} at ${fmt.offset(m.atMs)}`;
    const body = m.kind === "clip"
      ? `<video controls preload="metadata" ${m.poster ? `poster="${mediaUrl(m.poster)}"` : ""} src="${mediaUrl(m.path)}"></video>`
      : `<img src="${mediaUrl(m.path)}" alt="${cap}" loading="lazy">`;
    return `<div class="line is-media"><span class="line-time">${fmt.offset(m.atMs)}</span><figure class="media">${body}<figcaption>${cap}</figcaption></figure></div>`;
  }

  function lines(i) {
    if (i.state === "transcribing") return `<div class="skeleton"><div class="bone w85"></div><div class="bone w40"></div><div class="bone w60"></div><div class="bone w85"></div></div>`;
    const out = [];
    const posters = new Set(i.media.filter((m) => m.kind === "clip").map((m) => m.poster));
    const media = i.media.filter((m) => m.kind !== "audio" && !(m.kind === "frame" && posters.has(m.path))).sort((a, b) => a.atMs - b.atMs);
    let mi = 0;
    i.lines.forEach((l, idx) => {
      while (mi < media.length && media[mi].atMs <= l.startMs) out.push(mediaEl(media[mi++]));
      out.push(lineEl(i, l, idx));
    });
    while (mi < media.length) out.push(mediaEl(media[mi++]));
    if (!out.length) out.push(`<div class="empty">${i.state === "recording" ? "Nothing said yet." : "No transcript."}</div>`);
    return `<div class="lines">${out.join("")}</div>`;
  }

  function files(i) {
    const rows = [[`folder`, i.files.folder]];
    if (i.files.markdown) rows.push(["markdown", i.files.markdown]);
    if (i.share?.url) rows.push(["share", i.share.url]);
    return `<div class="files">${rows.map(([k, v]) => `<div><b>${k}</b> ${esc(v)}</div>`).join("")}</div>`;
  }

  function renderDetail() {
    const d = $("#detail");
    const i = items.find((x) => x.id === currentId);
    if (!i) { d.innerHTML = '<div class="blank">Pick something on the left.</div>'; return; }
    d.innerHTML = `<article class="page">
      ${kicker(i)}
      <h1 class="title">${esc(title(i))}</h1>
      <p class="when">${when(i)}</p>
      <div class="bar"><div class="bar-meta">${meta(i)}</div><div class="bar-actions">${actions(i)}</div></div>
      ${notice(i)}
      ${audio(i)}
      ${lines(i)}
      ${files(i)}
    </article>`;
  }
  $("#detail").addEventListener("click", onDetailClick);

  function onDetailClick(e) {
    const btn = e.target.closest("[data-act]");
    if (!btn) return;
    const i = items.find((x) => x.id === currentId);
    if (btn.dataset.act === "copy") copy(i.paste, btn, "Copied");
    if (btn.dataset.act === "copy-line") copy(i.lines[+btn.dataset.idx].text, btn, "Line copied");
    if (btn.dataset.act === "link") copy(i.share.url, btn, "Share link copied");
    if (btn.dataset.act === "finder") reveal(i.files.markdown || i.files.folder);
  }

  async function copy(text, btn, msg) {
    try {
      await tauri.core.invoke("library_copy", { text });
      const prev = btn.innerHTML;
      btn.classList.add("done");
      btn.innerHTML = ICON.check + (btn.querySelector("span") ? "<span>Copied</span>" : "");
      toast(msg);
      setTimeout(() => { btn.classList.remove("done"); btn.innerHTML = prev; }, 1400);
    } catch (error) { toast(`Copy failed: ${error}`); }
  }

  async function reveal(path) {
    try { await tauri.core.invoke("library_reveal", { path }); }
    catch (error) { toast(`Reveal failed: ${error}`); }
  }

  let toastTimer;
  function toast(msg) {
    const t = $("#toast");
    t.textContent = msg; t.classList.add("is-on");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => t.classList.remove("is-on"), 1500);
  }

  // Filters, search, keyboard
  for (const chip of document.querySelectorAll(".chip")) {
    chip.addEventListener("click", () => {
      kind = chip.dataset.kind;
      document.querySelectorAll(".chip").forEach((c) => c.classList.toggle("is-on", c === chip));
      const rows = visible();
      if (!rows.some((r) => r.id === currentId)) currentId = rows[0]?.id;
      renderList(); renderDetail();
    });
  }
  $("#search").addEventListener("input", (e) => { query = e.target.value; renderList(); });
  document.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") { e.preventDefault(); $("#search").focus(); $("#search").select(); return; }
    if (e.key === "Escape" && document.activeElement === $("#search")) { e.preventDefault(); $("#search").value = ""; query = ""; renderList(); return; }
    if (e.target.tagName === "INPUT" || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "j" && e.key !== "k") return;
    const rows = visible(); const idx = rows.findIndex((r) => r.id === currentId);
    const next = rows[idx + (e.key === "ArrowDown" || e.key === "j" ? 1 : -1)];
    if (next) { e.preventDefault(); select(next.id); $(`.row[data-id="${CSS.escape(next.id)}"]`)?.scrollIntoView({ block: "nearest" }); }
  });

  // Appearance
  const themes = ["auto", "light", "dark"];
  let theme = params.get("theme") || localStorage.getItem("see.theme") || "auto";
  function applyTheme() {
    document.documentElement.classList.add("is-switching");
    requestAnimationFrame(() => requestAnimationFrame(() => document.documentElement.classList.remove("is-switching")));
    if (theme === "auto") document.documentElement.removeAttribute("data-theme");
    else document.documentElement.dataset.theme = theme;
    $("#theme").textContent = `theme: ${theme}`;
    localStorage.setItem("see.theme", theme);
  }
  $("#theme").addEventListener("click", () => { theme = themes[(themes.indexOf(theme) + 1) % themes.length]; applyTheme(); });
  async function reload() {
    const version = ++loadVersion;
    const scroll = $("#list").scrollTop;
    try {
      const next = await tauri.core.invoke("library_items");
      if (version !== loadVersion) return;
      items = next.sort((a, b) => b.at - a.at);
      if (!currentId || !items.some((i) => i.id === currentId)) currentId = items[0]?.id ?? null;
      renderList();
      renderDetail();
      $("#list").scrollTop = scroll;
    } catch (error) { toast(`Library could not load: ${error}`); }
  }

  if (params.get("kind")) document.querySelector(`.chip[data-kind="${params.get("kind")}"]`)?.click();
  applyTheme();
  try { currentId = decodeURIComponent(location.hash.slice(1)) || null; } catch { currentId = null; }
  reload();
  tauri.event.listen("library-changed", reload);
  tauri.event.listen("library-select", ({ payload }) => select(payload));
  window.addEventListener("focus", reload);
})();
