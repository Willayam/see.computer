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
    dictionary: '<svg viewBox="0 0 16 16"><path d="M3 2.5h8.5a1.5 1.5 0 0 1 1.5 1.5v9.5H4.5A1.5 1.5 0 0 1 3 12z"/><path d="M3 12a1.5 1.5 0 0 1 1.5-1.5H13M6 5.5h4"/></svg>',
  };
  const ICON = {
    copy: '<svg viewBox="0 0 16 16"><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 5.5v-2a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2"/></svg>',
    check: '<svg viewBox="0 0 16 16"><path d="m3 8.5 3 3 7-7"/></svg>',
    finder: '<svg viewBox="0 0 16 16"><path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v6a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 12z"/></svg>',
    link: '<svg viewBox="0 0 16 16"><path d="M6.5 9.5a3 3 0 0 0 4.2 0l2-2a3 3 0 0 0-4.2-4.2l-1 1"/><path d="M9.5 6.5a3 3 0 0 0-4.2 0l-2 2a3 3 0 0 0 4.2 4.2l1-1"/></svg>',
    rec: '<svg viewBox="0 0 16 16"><circle cx="8" cy="8" r="3" fill="currentColor" stroke="none"/><circle cx="8" cy="8" r="6"/></svg>',
    spin: '<svg viewBox="0 0 16 16"><path d="M8 2a6 6 0 1 1-4.2 1.7"/></svg>',
    warn: '<svg viewBox="0 0 16 16"><path d="M8 2.5 14 13H2z"/><path d="M8 6.5v3M8 11.3v.2"/></svg>',
    edit: '<svg viewBox="0 0 16 16"><path d="M10.5 3 13 5.5 6 12.5H3.5V10z"/><path d="M9 4.5 11.5 7"/></svg>',
    plus: '<svg viewBox="0 0 16 16"><path d="M8 3v10M3 8h10"/></svg>',
    minus: '<svg viewBox="0 0 16 16"><path d="M3 8h10"/></svg>',
  };
  const NOUN = { dictation: "Dictations", take: "Screen take", meeting: "Meeting" };

  let kind = "all";
  let query = "";
  // Until the first load answers, the panes say so instead of sitting empty.
  let loaded = false;
  let currentId = null;
  // The transcript line being fixed, and the text typed so far, which a
  // reload must not throw away.
  let editing = null; // { id, idx, draft, error }
  let target = null; // { id, startMs }: a line opened from the Dictionary
  const dictionary = () => (kind === "dictionary" ? window.seeDictionary : null);

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
    if (i.state === "cloud") return "Downloading from iCloud…";
    if (i.kind === "dictation") return i.lines[0]?.text ?? "";
    if (i.kind === "take") return [fmt.dur(i.durationMs), `${i.media.filter((m) => m.kind !== "clip").length} captures`].filter(Boolean).join(" · ");
    if (i.state === "recording") return i.lines[0]?.text || "Recording…";
    if (i.state === "transcribing") return "Transcribing…";
    if (i.state === "failed") return i.error || "Transcription failed";
    return i.lines[0]?.text ?? "";
  }

  function renderList() {
    if (dictionary()) return dictionary().renderList();
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
      $(".row-time", el).textContent = i.state === "recording" ? "live" : i.state === "cloud" ? "" : i.kind === "dictation" ? `${fmt.clock(i.lines[0].startMs)}–${fmt.clock(i.lines.at(-1).startMs)}` : fmt.clock(i.at);
      el.addEventListener("click", () => select(i.id));
      list.appendChild(el);
    }
    if (!rows.length) list.innerHTML = `<div class="empty">${loaded ? "Nothing matches." : "Loading…"}</div>`;
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
      : i.state === "failed" ? '<span class="state bad">Failed</span>'
      : i.state === "cloud" ? '<span class="state busy">In iCloud</span>' : "";
    return `<div class="kicker">${GLYPH[i.kind]}<span>${NOUN[i.kind]}</span>${state ? '<span class="sep">·</span>' + state : ""}</div>`;
  }

  function title(i) {
    if (i.state === "cloud" && i.kind !== "dictation") return i.title;
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
    if (i.state === "cloud") return start;
    if (i.kind === "dictation") return `${start} · ${i.lines.length} entries from ${fmt.clock(i.lines[0].startMs)} to ${fmt.clock(i.lines.at(-1).startMs)}`;
    if (i.state === "recording") return `${start} · started ${fmt.clock(i.at)}`;
    if (i.durationMs == null) return `${start} · ${fmt.clock(i.at)}`;
    return `${start} · ${fmt.clock(i.at)} to ${fmt.clock(i.at + i.durationMs)}`;
  }

  function meta(i) {
    const parts = [];
    if (i.state === "cloud") return "";
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
    if (i.state === "cloud") return `<div class="notice">${ICON.spin}<div><strong>Downloading from iCloud.</strong> macOS moved this file to iCloud to free up space on this Mac. It appears here once it downloads.</div></div>`;
    if (i.state === "failed") return `<div class="notice bad">${ICON.warn}<div><strong>Transcription failed.</strong> ${esc(i.error || "The audio is still in the folder.")}</div></div>`;
    return "";
  }

  const fixable = (i) => i.kind === "dictation" || (i.kind === "meeting" && i.state === "done");

  function lineEl(i, l, idx) {
    const cls = ["line", `is-${i.kind}`];
    if (l.speaker) cls.push(`is-${l.speaker}`);
    if (target && target.id === i.id && target.startMs === l.startMs) cls.push("is-target");
    const time = i.kind === "dictation" ? fmt.clock(l.startMs) : fmt.offset(l.startMs);
    const speaker = l.speaker ? `<span class="speaker">${l.speaker === "you" ? "You" : "Them"}</span>` : "";
    if (editing && editing.id === i.id && editing.idx === idx) {
      cls.push("is-editing");
      return `<div class="${cls.join(" ")}" data-idx="${idx}">
        <span class="line-time">${time}</span>
        <div class="line-body">${speaker}
          <textarea class="fix" rows="3" spellcheck="true" aria-label="Fix this line">${esc(editing.draft)}</textarea>
          ${editing.error ? `<p class="fix-error" role="alert">${esc(editing.error)}</p>` : ""}
          <div class="fix-bar">
            <button class="act is-primary" data-act="fix-save">${ICON.check}<span>Save fix</span></button>
            <button class="act" data-act="fix-cancel"><span>Cancel</span></button>
            <span class="fix-hint">⌘↩ to save. see.computer learns the spellings you fix.</span>
          </div>
        </div>
      </div>`;
    }
    const edit = fixable(i) ? `<button class="line-edit" data-act="fix" data-idx="${idx}" title="Fix this line">${ICON.edit}</button>` : "";
    return `<div class="${cls.join(" ")}" data-idx="${idx}">
      <span class="line-time" title="${i.kind === "dictation" ? fmt.clockS(l.startMs) : ""}">${time}</span>
      <div class="line-body">${speaker}${esc(l.text)}</div>
      <div class="line-tools">${edit}<button class="line-copy" data-act="copy-line" data-idx="${idx}" title="Copy this line">${ICON.copy}</button></div>
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
    if (i.state === "transcribing" || i.state === "cloud") return `<div class="skeleton"><div class="bone w85"></div><div class="bone w40"></div><div class="bone w60"></div><div class="bone w85"></div></div>`;
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
    if (dictionary()) return dictionary().renderDetail();
    const d = $("#detail");
    const i = items.find((x) => x.id === currentId);
    if (!i) { d.innerHTML = `<div class="blank">${loaded ? "Pick something on the left." : "Loading your library…"}</div>`; return; }
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
    const fix = $(".fix", d);
    if (fix) {
      fix.addEventListener("input", () => { editing.draft = fix.value; });
      fix.addEventListener("keydown", (e) => {
        if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); saveFix(); }
        if (e.key === "Escape") { e.preventDefault(); editing = null; renderDetail(); }
      });
      fix.focus();
      fix.setSelectionRange(fix.value.length, fix.value.length);
    }
    const line = $(".line.is-editing, .line.is-target", d);
    if (line) line.scrollIntoView({ block: "center" });
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
    if (btn.dataset.act === "fix") startFix(i.id, +btn.dataset.idx);
    if (btn.dataset.act === "fix-save") saveFix();
    if (btn.dataset.act === "fix-cancel") { editing = null; renderDetail(); }
  }

  function startFix(id, idx) {
    const i = items.find((x) => x.id === id);
    if (!i || !i.lines[idx]) return;
    editing = { id, idx, draft: i.lines[idx].text, error: null };
    target = null;
    renderDetail();
  }

  async function saveFix() {
    const i = items.find((x) => x.id === editing?.id);
    const line = i?.lines[editing.idx];
    if (!line) { editing = null; renderDetail(); return; }
    try {
      await tauri.core.invoke("library_fix", { id: i.id, startMs: line.startMs, original: line.text, edited: editing.draft });
      editing = null;
      target = { id: i.id, startMs: line.startMs };
      toast("Fixed. see.computer learns from it.");
      await reload();
    } catch (error) {
      editing.error = String(error);
      await reload();
    }
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
  function setKind(next) {
    if (kind === "dictionary" && next !== "dictionary") window.seeDictionary.deactivate();
    kind = next;
    document.querySelectorAll(".chip").forEach((c) => {
      c.classList.toggle("is-on", c.dataset.kind === kind);
      c.setAttribute("aria-selected", String(c.dataset.kind === kind));
    });
    $("#search").placeholder = kind === "dictionary" ? "Search words" : "Search everything";
    if (dictionary()) { dictionary().activate(); return; }
    const rows = visible();
    if (!rows.some((r) => r.id === currentId)) currentId = rows[0]?.id;
    renderList(); renderDetail();
  }
  for (const chip of document.querySelectorAll(".chip")) {
    chip.addEventListener("click", () => setKind(chip.dataset.kind));
  }
  $("#search").addEventListener("input", (e) => { query = e.target.value; renderList(); });
  document.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") { e.preventDefault(); $("#search").focus(); $("#search").select(); return; }
    if (e.key === "Escape" && document.activeElement === $("#search")) { e.preventDefault(); $("#search").value = ""; query = ""; renderList(); return; }
    if (["INPUT", "TEXTAREA"].includes(e.target.tagName) || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "j" && e.key !== "k") return;
    if (dictionary()) { e.preventDefault(); dictionary().move(e.key === "ArrowDown" || e.key === "j" ? 1 : -1); return; }
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
      loaded = true;
      items = next.sort((a, b) => b.at - a.at);
      if (!currentId || !items.some((i) => i.id === currentId)) currentId = items[0]?.id ?? null;
      if (dictionary()) { dictionary().reload(); return; }
      renderList();
      // A fix being typed survives a reload unless its line is gone.
      const fix = $(".fix");
      const kept = editing && items.find((i) => i.id === editing.id)?.lines[editing.idx];
      if (editing && !kept) editing = null;
      if (!(fix && kept && !editing.error && document.activeElement === fix)) renderDetail();
      $("#list").scrollTop = scroll;
    } catch (error) { toast(`Library could not load: ${error}`); }
  }

  /* Routes the menu and the Dictionary open the Library on:
     "dictionary" or "dictionary:<term>", "edit:<item>@<startMs>" to fix a
     line, "<item>@<startMs>" to show one, or a bare item id. */
  async function route(path) {
    if (!path) return;
    if (path === "dictionary" || path.startsWith("dictionary:")) {
      setKind("dictionary");
      if (path.startsWith("dictionary:")) window.seeDictionary.select(path.slice(11));
      return;
    }
    const edit = path.startsWith("edit:");
    const [id, at] = (edit ? path.slice(5) : path).split("@");
    if (kind === "dictionary" || (kind !== "all" && !id.startsWith(kind))) setKind("all");
    if (!items.some((i) => i.id === id)) await reload();
    const i = items.find((x) => x.id === id);
    if (!i) return;
    currentId = id;
    const idx = at == null ? -1 : i.lines.findIndex((l) => l.startMs === +at);
    editing = null;
    target = idx >= 0 ? { id, startMs: i.lines[idx].startMs } : null;
    if (edit && idx >= 0 && fixable(i)) editing = { id, idx, draft: i.lines[idx].text, error: null };
    renderList();
    renderDetail();
    $(`.row[data-id="${CSS.escape(id)}"]`)?.scrollIntoView({ block: "nearest" });
  }

  // Marks the day header that is pinned over scrolled rows, for its fade.
  function markStuck() {
    const list = $("#list");
    const top = list.getBoundingClientRect().top;
    list.querySelectorAll(".day").forEach((h) => {
      h.classList.toggle("is-stuck", list.scrollTop > 0 && h.getBoundingClientRect().top - top < 1);
    });
  }
  $("#list").addEventListener("scroll", markStuck, { passive: true });
  new MutationObserver(markStuck).observe($("#list"), { childList: true });

  window.seeLibrary = {
    $, esc, fmt, toast, ICON, GLYPH,
    query: () => query,
    count: (text) => { $("#count").textContent = text; },
    open: (id, startMs) => route(startMs == null ? id : `${id}@${startMs}`),
  };

  document.addEventListener("DOMContentLoaded", async () => {
    applyTheme();
    let start = null;
    try { start = decodeURIComponent(location.hash.slice(1)) || null; } catch { start = null; }
    if (params.get("kind")) setKind(params.get("kind"));
    // The Dictionary loads on its own, so it opens without waiting for every
    // dictation, take and meeting to be read.
    const toDictionary = start === "dictionary" || !!start?.startsWith("dictionary:");
    if (toDictionary) await route(start);
    else { renderList(); renderDetail(); }
    await reload();
    if (!toDictionary) await route(start);
    tauri.event.listen("library-changed", reload);
    tauri.event.listen("library-select", ({ payload }) => route(payload));
    window.addEventListener("focus", reload);
  });
})();
