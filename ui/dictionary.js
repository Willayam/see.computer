// The Library's Dictionary section: the words in vocabulary.md and
// learned.md, drawn with the Library's own rows and pages. library.js owns
// the window, the chips, search and the theme, and hands the list and the
// detail pane to this file while the Dictionary chip is on.
(() => {
  const tauri = window.__TAURI__;
  const L = window.seeLibrary;
  const { $, esc, fmt, ICON, GLYPH } = L;
  const invoke = (cmd, args) => tauri.core.invoke(cmd, args);
  const keyOf = (term) => term.toLowerCase();
  const quoted = (text) => `“${text}”`;
  const plural = (n, one, many) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

  const view = {
    snap: null,
    selected: null, // { kind: "word" | "waiting" | "new", key }
    confirming: false,
    draftRule: null, // key of the word whose new heard form is being typed
    draftWord: "",
    message: null,
    active: false,
  };

  function matches(entry) {
    const q = L.query().trim().toLowerCase();
    if (!q) return true;
    return entry.term.toLowerCase().includes(q)
      || (entry.rules || []).some((r) => r.heard.toLowerCase().includes(q))
      || (entry.heard || "").toLowerCase().includes(q);
  }

  // The rows in the order the list shows them, so arrow keys and the
  // selection after a delete follow the screen.
  function visible() {
    if (!view.snap) return [];
    const words = view.snap.words.filter(matches);
    return [
      ...view.snap.waiting.filter(matches).map((w) => ({ kind: "waiting", key: keyOf(w.term), item: w, group: "Seen once" })),
      ...words.filter((w) => !w.parked).map((w) => ({ kind: "word", key: keyOf(w.term), item: w, group: "Words" })),
      ...words.filter((w) => w.parked).map((w) => ({ kind: "word", key: keyOf(w.term), item: w, group: "Parked" })),
    ];
  }

  // A fix waiting for review, or else the word that came up most recently.
  function opening() {
    const rows = visible();
    const heard = rows.filter((o) => o.kind === "word" && !o.item.parked && o.item.recent.length)
      .sort((a, b) => b.item.recent[0].atMs - a.item.recent[0].atMs);
    return rows.find((o) => o.kind === "waiting") || heard[0] || rows[0];
  }

  function find(sel) {
    if (!view.snap || !sel) return null;
    if (sel.kind === "waiting") return view.snap.waiting.find((w) => keyOf(w.term) === sel.key) || null;
    if (sel.kind === "word") return view.snap.words.find((w) => keyOf(w.term) === sel.key) || null;
    return null;
  }

  const same = (a, b) => a && b && a.kind === b.kind && a.key === b.key;

  function choose(sel) {
    blurPending();
    view.selected = sel;
    view.confirming = false;
    view.draftRule = null;
    view.message = null;
    renderList();
    renderDetail();
    $("#detail").scrollTop = 0;
  }

  function rowSub(o) {
    const w = o.item;
    if (o.kind === "waiting") return w.heard ? `heard ${quoted(w.heard)}` : "seen once";
    if (w.parked) return "parked, not boosted";
    if (!w.right && !w.misheard) return w.rules.length ? `heard as ${w.rules.map((r) => r.heard).join(", ")}` : "not heard yet";
    return `${w.right.toLocaleString()} right · ${w.misheard.toLocaleString()} misheard`;
  }

  function renderList() {
    const list = $("#list");
    const scroll = list.scrollTop;
    list.innerHTML = "";
    if (!view.snap) {
      if (!view.message) list.innerHTML = '<div class="empty">Loading your words…</div>';
      return;
    }
    const rows = visible();
    // Fixes are the first thing here. With none waiting, the list says
    // where they come from instead of opening on a word with no history.
    if (!view.snap.waiting.length && !L.query().trim() && rows.length) {
      list.insertAdjacentHTML("beforeend", '<div class="day">Seen once</div><p class="list-hint">Nothing to review. When you fix a word after dictating it, or fix a line in the Library, it shows up here.</p>');
    }
    let group = null;
    for (const o of rows) {
      if (o.group !== group) {
        const h = document.createElement("div");
        h.className = "day";
        h.textContent = o.group;
        list.appendChild(h);
        group = o.group;
      }
      const el = $("#t-row").content.firstElementChild.cloneNode(true);
      el.classList.toggle("is-on", same(view.selected, o));
      el.classList.toggle("is-parked", o.kind === "word" && o.item.parked);
      el.setAttribute("aria-selected", String(same(view.selected, o)));
      $(".row-glyph", el).innerHTML = GLYPH.dictionary;
      $(".row-title", el).textContent = o.item.term;
      $(".row-sub", el).textContent = rowSub(o);
      $(".row-time", el).textContent = o.kind === "word" && o.item.learned ? "learned" : o.kind === "waiting" ? "new" : "";
      el.addEventListener("click", () => choose({ kind: o.kind, key: o.key }));
      list.appendChild(el);
    }
    if (!rows.length) {
      const q = L.query().trim();
      list.innerHTML = q
        ? `<div class="empty">No words match ${esc(quoted(q))}.<br><button class="act" data-act="add-query">${ICON.plus}<span>Add ${esc(quoted(q))}</span></button></div>`
        : `<div class="empty">No words yet. Add the names and terms dictation gets wrong.</div>`;
      $('[data-act="add-query"]', list)?.addEventListener("click", () => startNew(q));
    }
    const active = view.snap.words.filter((w) => !w.parked).length;
    L.count(plural(active, "word", "words"));
    list.scrollTop = scroll;
    $(".row.is-on", list)?.scrollIntoView({ block: "nearest" });
  }

  function message() {
    return view.message ? `<div class="notice ${view.message.error ? "bad" : ""}" role="status">${ICON.warn}<div>${esc(view.message.text)}</div></div>` : "";
  }

  function source(w) {
    if (w.parked) return "Parked in vocabulary.md, so it isn’t boosted.";
    if (w.vocabulary && w.learned) return "In vocabulary.md, and learned from your fixes.";
    if (w.learned) return "Learned from your fixes and saved in learned.md.";
    return "In vocabulary.md.";
  }

  // The words around the first place the term or a heard form came up, so a
  // long dictation does not push the other lines off the page.
  function highlight(text, w) {
    const forms = [w.term, ...w.rules.map((r) => r.heard)].filter(Boolean)
      .map((f) => f.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
    const pattern = new RegExp(`(^|[^\\p{L}\\p{N}])(${forms.join("|")})(?=$|[^\\p{L}\\p{N}])`, "giu");
    const at = text.search(pattern);
    let snippet = text;
    if (text.length > 220 && at >= 0) {
      const start = at > 80 ? text.lastIndexOf(" ", at - 70) + 1 : 0;
      const end = text.indexOf(" ", Math.min(text.length, at + 140));
      snippet = (start > 0 ? "…" : "") + text.slice(start, end < 0 ? undefined : end) + (end < 0 ? "" : "…");
    }
    return esc(snippet).replace(pattern, (_, before, word) => `${before}<mark>${word}</mark>`);
  }

  function recentEl(w) {
    if (!w.recent.length) return `<p class="dict-help">Not seen in your ${plural(view.snap.lines, "saved line", "saved lines")} yet.</p>`;
    return `<div class="lines dict-recent">${w.recent.map((r, idx) => {
      const isMeeting = r.item.startsWith("meeting:");
      return `<button class="line is-dictation dict-hit" data-recent="${idx}" title="Open in the Library">
        <span class="line-time">${fmt.day(r.atMs)}<br>${fmt.clock(r.atMs)}</span>
        <span class="line-body"><span class="speaker ${r.misheard ? "is-miss" : "is-hit"}">${r.misheard ? "Misheard" : "Came out right"}${isMeeting ? " · meeting" : ""}</span>${highlight(r.text, w)}</span>
      </button>`;
    }).join("")}</div>`;
  }

  function rulesEl(w) {
    const rows = w.rules.map((r, idx) => `
      <div class="dict-rule">
        <input class="dict-field" value="${esc(r.heard)}" aria-label="Heard form ${idx + 1}" spellcheck="false" autocomplete="off" data-focus="rule-${esc(keyOf(r.heard))}" data-original="${esc(r.heard)}" data-rule="${idx}">
        <span class="dict-arrow">→ ${esc(w.term)}</span>
        ${r.learned ? '<span class="dict-tag">learned</span>' : ""}
        <button class="line-copy dict-remove" data-act="rule-remove" data-rule="${idx}" title="Remove this heard form" aria-label="Remove heard form ${esc(quoted(r.heard))}">${ICON.minus}</button>
      </div>`);
    if (view.draftRule === keyOf(w.term)) {
      rows.push(`<div class="dict-rule"><input class="dict-field" placeholder="superbase" aria-label="New heard form" spellcheck="false" autocomplete="off" data-focus="rule-new" data-original="" data-new-rule><span class="dict-arrow">→ ${esc(w.term)}</span></div>`);
    }
    return `<section class="dict-section">
      <h2 class="dict-h">Heard as</h2>
      <p class="dict-help">When the model writes one of these, see.computer writes ${esc(w.term)} instead.</p>
      ${rows.length ? `<div class="dict-rules">${rows.join("")}</div>` : '<p class="dict-help">None yet. Add one if the model keeps writing a different word.</p>'}
      ${view.draftRule ? "" : `<button class="act dict-add" data-act="rule-add">${ICON.plus}<span>Add heard form</span></button>`}
    </section>`;
  }

  function boostEl(w) {
    return `<section class="dict-section">
      <h2 class="dict-h"><label for="dict-boost">Boost</label></h2>
      <div class="dict-boost">
        <input id="dict-boost" class="dict-field" type="number" min="0.5" max="100" step="0.5" value="${w.weight}" data-focus="boost" data-original="${w.weight}" aria-describedby="dict-boost-help">
      </div>
      <p class="dict-help" id="dict-boost-help">1 is normal. A higher boost helps the model pick this word, and can pull similar-sounding words toward it.</p>
    </section>`;
  }

  function confirmEl(w) {
    const why = w.parked ? "It is removed from vocabulary.md."
      : w.learned ? "It stops being boosted, its heard forms stop being replaced, and see.computer won’t learn it again."
      : "It stops being boosted and its heard forms stop being replaced.";
    return `<div class="notice bad dict-confirm" role="group" aria-label="Confirm delete">${ICON.warn}<div>
      <strong>Delete ${esc(quoted(w.term))}?</strong> ${why}
      <div class="dict-confirm-bar"><button class="act is-danger" data-act="delete-yes"><span>Delete word</span></button><button class="act" data-act="delete-no"><span>Cancel</span></button></div>
    </div></div>`;
  }

  function wordPage(w) {
    const meta = w.parked ? ["<span>Parked</span>"]
      : [`<span>${w.right.toLocaleString()} right</span>`, `<span class="${w.misheard ? "them" : ""}">${w.misheard.toLocaleString()} misheard</span>`, `<span>Boost ${w.weight}</span>`];
    const acts = [];
    if (w.parked) acts.push(`<button class="act is-primary" data-act="unpark"><span>Turn on</span></button>`);
    else if (!w.learned) acts.push(`<button class="act" data-act="park" title="Keep it in vocabulary.md without boosting it"><span>Park</span></button>`);
    acts.push(`<button class="act" data-act="delete"><span>Delete…</span></button>`);
    acts.push(`<button class="act" data-act="reveal">${ICON.finder}<span>Reveal</span></button>`);
    const state = w.parked ? "Parked" : w.learned ? "Learned" : "";
    return `<article class="page">
      <div class="kicker">${GLYPH.dictionary}<span>Dictionary</span>${state ? `<span class="sep">·</span><span>${state}</span>` : ""}</div>
      <input class="title dict-title" value="${esc(w.term)}" aria-label="Word" spellcheck="false" autocomplete="off" ${w.parked ? "readonly" : ""} data-focus="title" data-original="${esc(w.term)}">
      <p class="when">${source(w)}</p>
      <div class="bar"><div class="bar-meta">${meta.join('<span class="sep">·</span>')}</div><div class="bar-actions">${acts.join("")}</div></div>
      ${view.confirming ? confirmEl(w) : ""}
      ${message()}
      ${w.parked ? "" : `<section class="dict-section"><h2 class="dict-h">Recent</h2>
        <p class="dict-help">Where it came up in your dictations and meetings, newest first. Open a line to fix it.</p>
        ${recentEl(w)}</section>`}
      ${w.parked ? "" : rulesEl(w)}
      ${w.parked ? "" : boostEl(w)}
    </article>`;
  }

  function waitingPage(w) {
    return `<article class="page">
      <div class="kicker">${GLYPH.dictionary}<span>Dictionary</span><span class="sep">·</span><span class="dict-new">Seen once</span></div>
      <h1 class="title">${esc(w.term)}</h1>
      <p class="when">${w.heard ? `You fixed ${esc(quoted(w.heard))} to ${esc(quoted(w.term))} once. A second fix teaches it automatically.` : "You fixed this spelling once. A second fix teaches it automatically."}</p>
      <div class="bar"><div class="bar-meta"><span>Waiting for a second fix</span></div><div class="bar-actions">
        <button class="act is-primary" data-act="learn"><span>Learn now</span></button>
        <button class="act" data-act="ignore"><span>Ignore</span></button>
      </div></div>
      ${message()}
      <p class="dict-help">Learning adds it to learned.md. Ignoring means see.computer won’t suggest it again.</p>
    </article>`;
  }

  function newPage() {
    return `<article class="page">
      <div class="kicker">${GLYPH.dictionary}<span>Dictionary</span></div>
      <h1 class="title">New word</h1>
      <form class="dict-new-form">
        <label class="dict-h" for="dict-new">Word</label>
        <input id="dict-new" class="dict-field dict-wide" value="${esc(view.draftWord)}" placeholder="Supabase" spellcheck="false" autocomplete="off" data-focus="new" aria-describedby="dict-new-help" ${view.message?.error ? 'aria-invalid="true"' : ""}>
        <p class="dict-help" id="dict-new-help">Write it the way it should appear. If the model hears it as something else, write heard -> Word, like superbase -> Supabase.</p>
        ${message()}
        <div class="dict-confirm-bar"><button class="act is-primary" type="submit"><span>Add word</span></button><button class="act" type="button" data-act="new-cancel"><span>Cancel</span></button></div>
      </form>
    </article>`;
  }

  // A redraw rebuilds the page, so the focused field, its caret and any text
  // not saved yet are carried across by the field's data-focus key, and the
  // blur that removing it fires is not taken as a commit.
  let rendering = false;
  function renderDetail() {
    const d = $("#detail");
    const active = document.activeElement;
    const focus = active && d.contains(active) && active.dataset.focus
      ? { key: active.dataset.focus, value: "original" in active.dataset && active.value !== active.dataset.original ? active.value : null, start: active.selectionStart, end: active.selectionEnd }
      : null;
    rendering = true;
    if (!view.snap) {
      d.innerHTML = view.message ? `<div class="blank-wrap">${message()}<button class="act" data-act="retry"><span>Try again</span></button></div>` : '<div class="blank">Loading your words…</div>';
    } else if (view.selected?.kind === "new") {
      d.innerHTML = newPage();
    } else {
      const item = find(view.selected);
      d.innerHTML = !item ? '<div class="blank">Pick a word on the left.</div>'
        : view.selected.kind === "waiting" ? waitingPage(item) : wordPage(item);
    }
    rendering = false;
    wire(d);
    if (focus) {
      const node = d.querySelector(`[data-focus="${CSS.escape(focus.key)}"]`);
      if (node) {
        if (focus.value != null) node.value = focus.value;
        node.focus();
        try { node.setSelectionRange(focus.start, focus.end); } catch { /* number inputs have no caret */ }
      }
    }
    if (view.selected?.kind === "new") $("#dict-new", d)?.focus();
    $('[data-new-rule]', d)?.focus();
    if (view.confirming) $('[data-act="delete-no"]', d)?.focus();
  }

  function commitOnBlur(input, commit, revert) {
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); input.blur(); }
      if (e.key === "Escape") { e.preventDefault(); revert(); input.blur(); }
    });
    input.addEventListener("blur", () => { if (!rendering) commit(); });
  }

  function wire(d) {
    const word = view.selected?.kind === "word" ? find(view.selected) : null;
    const title = $(".dict-title", d);
    if (title && word && !word.parked) {
      commitOnBlur(title, () => {
        const to = title.value.trim();
        if (!to) { title.value = word.term; return; }
        if (to !== word.term) edit({ op: "rename", term: word.term, to }, { kind: "word", key: keyOf(to) });
      }, () => { title.value = word.term; });
    }
    d.querySelectorAll("[data-rule]").forEach((input) => {
      if (input.tagName !== "INPUT") return;
      const rule = word.rules[+input.dataset.rule];
      commitOnBlur(input, () => {
        const to = input.value.trim();
        if (!to) { input.value = rule.heard; return; }
        if (to !== rule.heard) edit({ op: "rule", term: word.term, from: rule.heard, to });
      }, () => { input.value = rule.heard; });
    });
    const fresh = $("[data-new-rule]", d);
    if (fresh) {
      let done = false;
      const finish = (save) => {
        if (done) return;
        done = true;
        const to = fresh.value.trim();
        view.draftRule = null;
        if (save && to) edit({ op: "rule", term: word.term, from: null, to });
        else renderDetail();
      };
      fresh.addEventListener("keydown", (e) => {
        if (e.key === "Enter") { e.preventDefault(); finish(true); }
        if (e.key === "Escape") { e.preventDefault(); finish(false); }
      });
      fresh.addEventListener("blur", () => { if (!rendering) finish(true); });
    }
    const boost = $("#dict-boost", d);
    if (boost) {
      let timer = null;
      const commit = () => {
        clearTimeout(timer);
        const weight = Number(boost.value);
        if (boost.value && Number.isFinite(weight) && weight !== word.weight) edit({ op: "weight", term: word.term, weight });
      };
      boost.addEventListener("input", () => { clearTimeout(timer); timer = setTimeout(commit, 600); });
      boost.addEventListener("change", commit);
    }
    const form = $(".dict-new-form", d);
    if (form) {
      const input = $("#dict-new", d);
      input.addEventListener("input", () => { view.draftWord = input.value; });
      input.addEventListener("keydown", (e) => { if (e.key === "Escape") { e.preventDefault(); cancelNew(); } });
      form.addEventListener("submit", (e) => {
        e.preventDefault();
        const text = input.value.trim();
        if (!text) { view.message = { text: "Type a word to add.", error: true }; renderDetail(); return; }
        const term = text.includes(" -> ") ? text.split(" -> ").pop().trim() : text;
        edit({ op: "add", text }, { kind: "word", key: keyOf(term) }, () => { view.draftWord = ""; });
      });
    }
    d.querySelectorAll("[data-recent]").forEach((button) => {
      const r = word.recent[+button.dataset.recent];
      button.addEventListener("click", () => L.open(r.item, r.startMs));
    });
  }

  function onClick(e) {
    if (!view.active) return;
    const btn = e.target.closest("[data-act]");
    if (!btn || !$("#detail").contains(btn)) return;
    const item = find(view.selected);
    const act = btn.dataset.act;
    if (act === "retry") reload();
    if (act === "new-cancel") cancelNew();
    if (!item) return;
    if (act === "learn") edit({ op: "learn", term: item.term }, { kind: "word", key: keyOf(item.term) });
    if (act === "ignore") edit({ op: "ignore", term: item.term }, neighbour());
    if (act === "park") edit({ op: "park", term: item.term });
    if (act === "unpark") edit({ op: "unpark", term: item.term });
    if (act === "delete") { view.confirming = true; renderDetail(); }
    if (act === "delete-no") { view.confirming = false; renderDetail(); }
    if (act === "delete-yes") edit({ op: "delete", term: item.term }, neighbour());
    if (act === "rule-add") { view.draftRule = keyOf(item.term); renderDetail(); }
    if (act === "rule-remove") edit({ op: "rule", term: item.term, from: item.rules[+btn.dataset.rule].heard, to: null });
    if (act === "reveal") invoke("dictionary_reveal").catch((error) => L.toast(String(error)));
  }

  // Something in the page holds typing the files do not have yet; a reload
  // then would throw it away, so it waits until that field commits.
  function editing() {
    const a = document.activeElement;
    return !!(a && $("#detail").contains(a) && a.tagName === "INPUT" && "original" in a.dataset && a.value !== a.dataset.original)
      || !!view.draftRule || (view.selected?.kind === "new" && !!view.draftWord);
  }

  function blurPending() {
    const a = document.activeElement;
    if (editing() && a && $("#detail").contains(a)) a.blur();
  }

  function accept(snap, selection) {
    const before = view.selected;
    view.snap = snap;
    if (selection !== undefined) view.selected = selection;
    if (view.selected && view.selected.kind !== "new" && !find(view.selected)) view.selected = null;
    if (!view.selected) {
      const first = opening();
      view.selected = first ? { kind: first.kind, key: first.key } : null;
    }
    // A confirmation belongs to the word it was opened for, never the next.
    if (!same(before, view.selected)) view.confirming = false;
    if (view.active) { renderList(); renderDetail(); }
  }

  // Edits run one at a time in the order made, so a heard form typed while
  // the boost is still saving is not lost. Where the selection lands after an
  // edit is the edit's call, unless the user has already moved on.
  let queue = Promise.resolve();
  function edit(op, selection, done) {
    const from = view.selected;
    queue = queue.then(() => run(op, view.selected === from ? selection : undefined, done));
    return queue;
  }

  async function run(op, selection, done) {
    view.message = null;
    try {
      const snap = await invoke("dictionary_edit", { edit: op });
      view.confirming = false;
      if (done) done();
      accept(snap, selection);
      L.toast(op.op === "learn" ? `Learned ${quoted(op.term)}. It applies to your next dictation.` : "Saved. It applies to your next dictation.");
    } catch (error) {
      view.confirming = false;
      view.message = { text: String(error), error: true };
      await reload();
    }
  }

  function neighbour() {
    const rows = visible();
    const index = rows.findIndex((o) => same(o, view.selected));
    const next = rows[index + 1] || rows[index - 1];
    return next ? { kind: next.kind, key: next.key } : null;
  }

  function startNew(prefill = "") {
    view.draftWord = prefill;
    choose({ kind: "new", key: "" });
  }

  function cancelNew() {
    view.draftWord = "";
    view.selected = null;
    view.message = null;
    accept(view.snap);
  }

  async function reload() {
    try {
      accept(await invoke("dictionary_load"));
    } catch (error) {
      view.message = { text: String(error), error: true };
      if (view.active) renderDetail();
    }
  }

  async function poll() {
    if (view.active && document.visibilityState === "visible") {
      try {
        if (!view.snap) await reload();
        else if ((await invoke("dictionary_stamp")) !== view.snap.stamp && !editing()) await reload();
      } catch { /* the next poll tries again */ }
    }
    setTimeout(poll, 1500);
  }

  $("#detail").addEventListener("click", onClick);
  document.addEventListener("keydown", (e) => {
    if (view.active && (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "n") { e.preventDefault(); startNew(L.query().trim()); }
  });
  $("#add-word").addEventListener("click", () => startNew(L.query().trim()));

  window.seeDictionary = {
    activate() {
      view.active = true;
      document.body.classList.add("is-dictionary");
      renderList();
      renderDetail();
      reload();
    },
    deactivate() {
      view.active = false;
      document.body.classList.remove("is-dictionary");
    },
    renderList,
    renderDetail,
    reload,
    select(term) {
      const key = keyOf(term);
      const run = () => {
        const sel = view.snap?.waiting.some((w) => keyOf(w.term) === key) ? { kind: "waiting", key } : { kind: "word", key };
        if (find(sel)) choose(sel);
      };
      if (view.snap) run(); else reload().then(run);
    },
    move(delta) {
      const rows = visible();
      const index = rows.findIndex((o) => same(o, view.selected));
      const next = rows[Math.max(0, Math.min(rows.length - 1, index + delta))];
      if (next) choose({ kind: next.kind, key: next.key });
    },
  };
  setTimeout(poll, 1500);
})();
