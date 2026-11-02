(() => {
  const L = window.seeLibrary;
  const { $, esc, ICON, GLYPH } = L;
  const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
  const keyOf = (row) => JSON.stringify([row.term.toLowerCase(), row.heard?.toLowerCase() ?? null]);
  const view = { snap: null, selected: null, active: false, message: "", draft: null };
  let loading = null;
  let queue = Promise.resolve();
  let pending = 0;

  function visible() {
    const q = L.query().trim().toLowerCase();
    return (view.snap?.rows || []).filter((r) => r.term.toLowerCase().includes(q) || (r.heard || "").toLowerCase().includes(q));
  }

  function selected() {
    return view.snap?.rows.find((r) => keyOf(r) === view.selected);
  }

  function renderList() {
    const list = $("#list");
    const scroll = list.scrollTop;
    list.innerHTML = "";
    if (!view.snap) {
      list.innerHTML = '<div class="empty">Loading your words…</div>';
      return;
    }
    for (const row of visible()) {
      const el = $("#t-row").content.firstElementChild.cloneNode(true);
      el.classList.toggle("is-on", keyOf(row) === view.selected);
      el.setAttribute("aria-selected", String(keyOf(row) === view.selected));
      $(".row-glyph", el).innerHTML = GLYPH.dictionary;
      $(".row-title", el).textContent = row.heard ? `${row.heard} → ${row.term}` : row.term;
      $(".row-sub", el).remove();
      if (row.learned) {
        const time = $(".row-time", el);
        time.innerHTML = ICON.sparkle;
        time.title = "Learned from your fixes";
        time.setAttribute("aria-label", time.title);
      }
      el.addEventListener("click", () => choose(keyOf(row)));
      list.appendChild(el);
    }
    const q = L.query().trim();
    if (!visible().length) {
      list.innerHTML = q
        ? `<div class="empty">No words match “${esc(q)}”. Press ⌘N to add it.</div>`
        : '<div class="empty">No words yet. Add the names and terms dictation gets wrong.</div>';
    }
    const total = view.snap.rows.length;
    L.count(`${total.toLocaleString()} ${total === 1 ? "entry" : "entries"}`);
    list.scrollTop = scroll;
  }

  function choose(key) {
    document.activeElement?.blur();
    view.selected = key;
    view.draft = key === "new" ? { term: L.query().trim(), heard: "" } : null;
    view.message = "";
    renderList();
    renderDetail();
    $("#detail").scrollTop = 0;
  }

  // Tabbing from one field to the other saves and redraws the page, so the
  // focus and caret are carried across and the redraw's own blur saves nothing.
  let redrawing = false;
  function renderDetail() {
    const detail = $("#detail");
    const active = document.activeElement;
    const focus = active && detail.contains(active) && active.id
      ? { id: active.id, start: active.selectionStart, end: active.selectionEnd }
      : null;
    redrawing = true;
    drawDetail(detail);
    redrawing = false;
    if (focus) {
      const node = detail.querySelector(`#${focus.id}`);
      node?.focus();
      node?.setSelectionRange(focus.start, focus.end);
    }
  }

  function drawDetail(detail) {
    if (!view.snap) {
      detail.innerHTML = `<div class="blank">${esc(view.message || "Loading your words…")}</div>`;
      return;
    }
    const row = selected();
    const fresh = view.selected === "new";
    if (!row && !fresh) {
      detail.innerHTML = '<div class="blank">Pick a word on the left.</div>';
      return;
    }
    const draft = view.draft || { term: row.term, heard: row.heard || "" };
    detail.innerHTML = `<article class="page">
      <div class="kicker">${GLYPH.dictionary}<span>Dictionary</span>${row?.learned ? '<span class="sep">·</span><span>Learned</span>' : ""}</div>
      <h1 class="title">${fresh ? "New word" : esc(row.term)}</h1>
      <form class="dict-form">
        <label class="dict-h" for="dict-word">Word</label>
        <input id="dict-word" class="dict-field dict-wide" required value="${esc(draft.term)}" placeholder="Supabase" spellcheck="false" autocomplete="off">
        <label class="dict-h" for="dict-heard">Replaces</label>
        <input id="dict-heard" class="dict-field dict-wide" value="${esc(draft.heard)}" placeholder="superbase" spellcheck="false" autocomplete="off" aria-describedby="dict-help">
        <p id="dict-help" class="dict-help">What the model writes instead. Leave it empty to only boost the word.</p>
        ${view.message ? `<p class="notice bad" role="status">${esc(view.message)}</p>` : ""}
        <div class="bar-actions">${fresh
          ? '<button class="act is-primary" type="submit">Add word</button><button class="act" type="button" data-cancel>Cancel</button>'
          : `<button class="act is-danger" type="button" data-delete>Delete</button><button class="act" type="button" data-reveal>${ICON.finder}<span>Reveal</span></button>`}
        </div>
      </form>
    </article>`;
    const word = $("#dict-word", detail);
    const heard = $("#dict-heard", detail);
    const form = $("form", detail);
    let saving = false;
    function save() {
      if (redrawing || saving || !form.reportValidity()) return;
      const next = { term: word.value.trim(), heard: heard.value.trim() || null };
      if (!next.term) return;
      if (!fresh && next.term === row.term && next.heard === row.heard) { view.draft = null; return; }
      saving = true;
      const text = next.heard ? `${next.heard} -> ${next.term}` : next.term;
      edit(fresh ? { op: "add", text } : { op: "change", term: row.term, heard: row.heard, to: text }, keyOf(next), next);
    }
    for (const input of [word, heard]) {
      input.addEventListener("input", () => { view.draft = { term: word.value, heard: heard.value }; });
      input.addEventListener("keydown", (e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          word.value = fresh ? "" : row.term;
          heard.value = fresh ? "" : row.heard || "";
          view.draft = null;
          if (fresh) choose(null);
          else input.blur();
        }
        if (e.key === "Enter" && !fresh) { e.preventDefault(); save(); }
      });
      if (!fresh) input.addEventListener("blur", save);
    }
    form.addEventListener("submit", (e) => { e.preventDefault(); save(); });
    $("[data-cancel]", detail)?.addEventListener("click", () => choose(null));
    $("[data-delete]", detail)?.addEventListener("click", () => {
      const rows = visible();
      const i = rows.findIndex((r) => keyOf(r) === keyOf(row));
      const next = rows[i + 1] || rows[i - 1];
      edit({ op: "delete", term: row.term, heard: row.heard }, next ? keyOf(next) : null);
    });
    $("[data-reveal]", detail)?.addEventListener("click", () => invoke("dictionary_reveal").catch((e) => L.toast(String(e))));
    if (fresh) word.focus();
  }

  function accept(snap, selection) {
    view.snap = snap;
    if (selection !== undefined) view.selected = selection;
    if (view.selected !== "new" && !selected()) view.selected = visible()[0] ? keyOf(visible()[0]) : null;
    if (view.active) { renderList(); renderDetail(); }
  }

  function edit(op, selection, submitted) {
    const from = view.selected;
    pending++;
    queue = queue.then(async () => {
      try {
        const snap = await invoke("dictionary_edit", { edit: op });
        const stillHere = view.selected === from;
        const draft = stillHere ? view.draft : null;
        const changed = submitted && draft
          && (draft.term.trim() !== submitted.term || (draft.heard.trim() || null) !== submitted.heard);
        if (stillHere && !changed) view.draft = null;
        view.message = "";
        accept(snap, stillHere ? selection : undefined);
        if (changed && draft.term.trim()) {
          const next = { term: draft.term.trim(), heard: draft.heard.trim() || null };
          const to = next.heard ? `${next.heard} -> ${next.term}` : next.term;
          edit({ op: "change", term: submitted.term, heard: submitted.heard, to }, keyOf(next), next);
        }
        L.toast("Saved. It applies to your next dictation.");
      } catch (error) {
        view.message = String(error);
        await reload();
      } finally { pending--; }
    });
    return queue;
  }

  function reload() {
    return (loading ??= invoke("dictionary_load").then((snap) => accept(snap), (error) => {
      view.message = String(error);
      if (view.active) renderDetail();
    }).finally(() => { loading = null; }));
  }

  async function poll() {
    if (view.active && document.visibilityState === "visible" && !view.draft && !pending) {
      try {
        if (!view.snap || await invoke("dictionary_stamp") !== view.snap.stamp) await reload();
      } catch { /* Retry on the next poll. */ }
    }
    setTimeout(poll, 1500);
  }

  $("#add-word").addEventListener("click", () => choose("new"));
  document.addEventListener("keydown", (e) => {
    if (view.active && (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "n") { e.preventDefault(); choose("new"); }
  });
  window.seeDictionary = {
    activate() {
      view.active = true;
      document.body.classList.add("is-dictionary");
      renderList();
      renderDetail();
      reload();
    },
    deactivate() { view.active = false; document.body.classList.remove("is-dictionary"); },
    renderList, renderDetail, reload,
    select(term) {
      reload().then(() => {
        const row = view.snap?.rows.find((r) => r.term.toLowerCase() === term.toLowerCase());
        if (row) choose(keyOf(row));
      });
    },
    move(delta) {
      const rows = visible();
      const i = rows.findIndex((r) => keyOf(r) === view.selected);
      const next = rows[Math.max(0, Math.min(rows.length - 1, i + delta))];
      if (next) choose(keyOf(next));
    },
  };
  setTimeout(poll, 1500);
})();
