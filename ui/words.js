(() => {
  const L = window.seeLibrary;
  const { $, esc } = L;
  const tauri = window.__TAURI__;
  const WORDS = "words";
  const LINES = '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 4h11M2.5 8h11M2.5 12h6.5"/></svg>';
  const EASE = "cubic-bezier(.2, .8, .2, 1)";
  const PLAY_MS = 1100;
  // The dial runs from 0 to 4x typing; one unit of its path is one wpm.
  const DIAL_MAX = 160;
  const WEEKS = 13;

  const LEVEL_ICON = [
    "",
    '<rect x="6" y="3.5" width="12" height="17" rx="2"/><path d="M9.5 8.5h5M9.5 12h5M9.5 15.5h2.5"/>',
    '<path d="M8 4.5V4a1.5 1.5 0 0 1 1.5-1.5h8A1.5 1.5 0 0 1 19 4v12a1.5 1.5 0 0 1-1.5 1.5H17"/><rect x="5" y="5.5" width="11" height="16" rx="1.8"/><path d="M8 10h5M8 13.5h5M8 17h2.5"/>',
    '<rect x="7.5" y="3" width="9" height="18" rx="1.6"/><path d="M10.5 3v18M13 7.5h1"/>',
    '<path d="M12 6.6C9.9 5.3 7.3 4.9 4 5v13c3.3-.1 5.9.3 8 1.6 2.1-1.3 4.7-1.7 8-1.6V5c-3.3-.1-5.9.3-8 1.6z"/><path d="M12 6.6v13"/>',
    '<rect x="4.5" y="3" width="15" height="18" rx="1.8"/><path d="M8.5 3v18M12 7.5h4M12 11h4"/><path d="M14 21v-4.5l1.5 1.2 1.5-1.2V21"/>',
    '<rect x="3.5" y="6" width="5" height="15" rx="1.2"/><rect x="9.5" y="3" width="5" height="18" rx="1.2"/><rect x="15.5" y="8" width="5" height="13" rx="1.2"/><path d="M5 10h2M11 7h2M17 12h2"/>',
    '<path d="M2.5 20.5h19"/><rect x="4" y="6.5" width="3.6" height="14" rx="1"/><rect x="7.6" y="4" width="3.6" height="16.5" rx="1"/><rect x="11.2" y="8" width="3.6" height="12.5" rx="1"/><path d="M16.3 8.3l2.9-.9 3 12.2-2.9.9z"/>',
  ];
  const SPEED_ICON = {
    2: '<path d="M8 7l5 5-5 5M14 7l5 5-5 5"/>',
    3: '<path d="M5.5 7l5 5-5 5M11 7l5 5-5 5M16.5 7l5 5-5 5"/>',
    4: '<path d="M3.5 7l5 5-5 5M8.5 7l5 5-5 5M13.5 7l5 5-5 5M18.5 7l5 5-5 5"/>',
  };

  const state = { data: null, drawn: null, shown: {}, shownWpm: null, play: false, error: "" };
  let loading = null;
  let reloadPending = false;

  const still = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
  const n = (v, dec = 0) => {
    const [a, b] = Number(v).toFixed(dec).split(".");
    return a.replace(/\B(?=(\d{3})+$)/g, ",") + (b ? `.${b}` : "");
  };
  const plural = (count, one, many) => `${n(count)} ${count === 1 ? one : many}`;
  const WD = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const MON = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const day = (iso) => new Date(`${iso}T00:00:00Z`);
  const iso = (d) => d.toISOString().slice(0, 10);
  const short = (s) => { const d = day(s); return `${d.getUTCDate()} ${MON[d.getUTCMonth()]}`; };
  const named = (s) => `${WD[day(s).getUTCDay()]} ${short(s)}`;
  // A number that counts up on open and ticks to its new value on a refresh.
  const count = (key, to, dec = 0, from, tag = "span") =>
    `<${tag} data-n="${key}" data-to="${to}" data-dec="${dec}"${from == null ? "" : ` data-from="${from}"`}>${n(to, dec)}</${tag}>`;

  function row(on) {
    const el = $("#t-row").content.firstElementChild.cloneNode(true);
    el.dataset.id = WORDS;
    el.id = "words-row";
    el.classList.toggle("is-on", on);
    el.setAttribute("aria-selected", String(on));
    $(".row-glyph", el).innerHTML = LINES;
    $(".row-title", el).textContent = "Words";
    $(".row-sub", el).textContent = rowSub();
    el.addEventListener("click", () => L.open(WORDS));
    return el;
  }

  function rowSub() {
    const d = state.data;
    if (!d?.history || !d.since || !d.total) return "";
    return d.wpm == null ? n(d.total) : `${n(d.total)} · ${d.wpm} wpm`;
  }

  function gauge(d) {
    const w = Math.min(d.wpm ?? 0, DIAL_MAX);
    const a = (210 - (w / DIAL_MAX) * 240) * Math.PI / 180;
    const p = (r) => `${(160 + r * Math.cos(a)).toFixed(1)} ${(152 - r * Math.sin(a)).toFixed(1)}`;
    const label = d.wpm == null ? "Not timed yet" : `${d.wpm} words a minute, ${d.timesTyping} times a ${d.typingWpm} words a minute typist`;
    return `<figure class="gauge">
      <svg viewBox="0 0 320 222" role="img" aria-label="${esc(label)}">
        <path class="g-track" d="M63 208A112 112 0 1 1 257 208"/>
        ${d.wpm == null ? "" : `<path class="g-fill" d="M63 208A112 112 0 1 1 257 208" pathLength="${DIAL_MAX}" stroke-dasharray="${w} 400"/>`}
        <path class="g-type" d="M81.2 197.5A91 91 0 0 1 81.2 106.5" pathLength="40"/>
        <path class="g-gap" d="M71.7 101L54.3 91"/>
        <path class="g-gap" d="M160 50L160 30"/>
        <path class="g-gap" d="M248.3 101L265.7 91"/>
        <text class="g-tick" x="44.8" y="222">0</text>
        <text class="g-tick" x="44.8" y="89">1x</text>
        <text class="g-tick" x="160" y="22.5">2x</text>
        <text class="g-tick" x="275.2" y="89">3x</text>
        <text class="g-tick" x="275.2" y="222">4x</text>
        ${d.wpm == null ? "" : `<g class="g-needle"><path d="M${p(99)}L${p(125)}"/></g>`}
        <text class="g-n" x="160" y="160">${d.wpm == null ? "" : count("wpm", d.wpm, 0, d.typingWpm, "tspan")}</text>
        <text class="g-u" x="160" y="181">${d.wpm == null ? "Not timed yet" : "words a minute"}</text>
        <text class="g-x" x="160" y="200">${d.wpm == null ? "" : `${count("times", d.timesTyping, 1, 1, "tspan")}x typing speed`}</text>
      </svg>
      <figcaption class="g-key">${d.wpm == null ? "" : `<span class="you"><i></i>You, ${d.wpm} wpm</span>`}<span><i></i>Typing, ${d.typingWpm} wpm</span></figcaption>
    </figure>`;
  }

  function saved(d) {
    const most = Math.max(d.hoursTyped || 0, d.hoursSpoken || 0) || 1;
    return `<div class="saved">
      <div class="k">Time saved</div>
      <div class="saved-n">${count("saved", d.hoursSaved ?? 0, 1)}<span class="unit"> hours</span></div>
      <div class="tbar"><span>Typed</span><span class="tb"><i class="t-typed" data-w="${(d.hoursTyped / most) * 100}"></i></span><span class="tv">${n(d.hoursTyped, 1)} h</span></div>
      <div class="tbar"><span>Spoken</span><span class="tb"><i class="t-spoken" data-w="${((d.hoursSpoken ?? 0) / most) * 100}"></i></span><span class="tv">${d.hoursSpoken == null ? "" : `${n(d.hoursSpoken, 1)} h`}</span></div>
      <p class="saved-note">What typing these words at ${d.typingWpm} wpm would have taken.</p>
    </div>`;
  }

  function tiles(d) {
    const t = d.today, w = d.last7;
    const todaySub = t.wpm == null ? "" : `${t.wpm} wpm, ${n(t.wpm / d.typingWpm, 1)}x typing`;
    return `<dl class="w-tiles">
      <div><dt class="k">All time</dt><dd>${count("total", d.total)}<small>words</small></dd><p>On your Macs since ${short(d.since)}</p></div>
      <div><dt class="k">Today</dt><dd>${count("today", t.words)}<small>words</small></dd><p>${todaySub}</p></div>
      <div><dt class="k">Last 7 days</dt><dd>${count("last7", w.words)}<small>words</small></dd><p>${n(w.perDay)} a day</p></div>
    </dl>`;
  }

  function tip(d, x) {
    const parts = [`${x.date === d.today.date ? "Today, " : ""}${named(x.date)}`, plural(x.words, "word", "words")];
    if (x.wpm != null) parts.push(`${x.wpm} wpm`);
    return parts.join(" · ");
  }

  function rhythm(d) {
    const byDate = new Map(d.days.map((x) => [x.date, x]));
    const fast = (x) => x.wpm != null && x.wpm >= d.typingWpm * 3;
    const most = Math.max(1, ...d.days.map((x) => x.words));
    const today = day(d.today.date);
    const start = new Date(today);
    start.setUTCDate(today.getUTCDate() - ((today.getUTCDay() + 6) % 7) - (WEEKS - 1) * 7);
    const out = ['<text x="0" y="34">Mon</text><text x="0" y="84">Wed</text><text x="0" y="134">Fri</text><text x="0" y="184">Sun</text>'];
    for (let col = 0; col < WEEKS; col++) {
      const cx = 38.5 + col * 25;
      const monday = new Date(start); monday.setUTCDate(start.getUTCDate() + col * 7);
      for (let r = 0; r < 7; r++) {
        const date = new Date(monday); date.setUTCDate(monday.getUTCDate() + r);
        if (date > today) break;
        if (col === 0 && r === 0 || date.getUTCDate() === 1) {
          out.push(`<text x="${cx - 9.5}" y="10">${MON[date.getUTCMonth()]}</text>`);
        }
        const cy = 30.5 + r * 25;
        const x = byDate.get(iso(date));
        if (!x || !x.words) { out.push(`<circle class="r-zero c${col}" cx="${cx}" cy="${cy}" r="1.3"/>`); continue; }
        const rad = +(1.6 + 9.6 * Math.sqrt(x.words / most)).toFixed(1);
        if (x.date === d.bestDay?.date || x.date === d.fastestDay?.date) {
          out.push(`<circle class="r-mark c${col}" cx="${cx}" cy="${cy}" r="${(rad + 2.6).toFixed(1)}"/>`);
        }
        const t = esc(tip(d, x));
        out.push(`<circle class="r-day c${col}${fast(x) ? " is-fast" : ""}" cx="${cx}" cy="${cy}" r="${rad}" data-tip="${t}"><title>${t}</title></circle>`);
      }
    }
    const fastDays = d.days.filter(fast).length;
    const facts = [
      `<li><svg viewBox="0 0 14 14"><circle class="blue" cx="7" cy="7" r="5"/></svg><span>3x typing or faster<b>${plural(fastDays, "day", "days")}</b></span></li>`,
    ];
    if (d.bestDay) facts.push(`<li><svg viewBox="0 0 14 14"><circle class="grey" cx="7" cy="7" r="5"/><circle class="ring" cx="7" cy="7" r="6.4"/></svg><span>Best day, ${named(d.bestDay.date)}<b>${plural(d.bestDay.words, "word", "words")}</b></span></li>`);
    if (d.fastestDay) facts.push(`<li><svg viewBox="0 0 14 14"><circle class="blue" cx="7" cy="7" r="2.6"/><circle class="ring" cx="7" cy="7" r="5"/></svg><span>Fastest day, ${named(d.fastestDay.date)}<b>${d.fastestDay.wpm} wpm</b></span></li>`);
    facts.push(`<li><svg viewBox="0 0 14 14"><circle class="grey" cx="4" cy="7" r="1.6"/><circle class="grey" cx="10" cy="7" r="3.4"/></svg><span>Days with dictation<b>${n(d.activeDays)}</b></span></li>`);
    const now = byDate.get(d.today.date) || { date: d.today.date, words: d.today.words, wpm: d.today.wpm };
    return `<section class="w-rhythm">
      <div class="k">Every day</div>
      <div class="r-body">
        <svg class="rhythm" viewBox="0 0 351 193" role="img" aria-label="One dot for each day of the last ${WEEKS} weeks, sized by words dictated">${out.join("")}</svg>
        <div class="r-side">
          <ul class="r-facts">${facts.join("")}</ul>
          <p class="r-read">${esc(tip(d, now))}</p>
        </div>
      </div>
    </section>`;
  }

  function pace(d) {
    const next = d.level.next;
    if (!next) return "";
    const perDay = d.last7.words / 7;
    let when = "";
    if (perDay > 0) {
      const days = Math.max(1, Math.round(next.toGo / perDay));
      const weeks = Math.round(days / 7);
      when = days < 14 ? `, about ${plural(days, "day", "days")}` : `, about ${n(weeks)} weeks`;
      when += " at this week's pace";
    }
    return `${n(next.toGo)} to ${esc(next.name)}${when}`;
  }

  function levels(d) {
    const now = d.level.index, next = d.level.next;
    const track = d.levels.map((l) => {
      const cls = [l.reached ? "got" : "", l.index === now ? "now" : ""].filter(Boolean).join(" ");
      const when = l.reached ? `<time>${short(l.reached)}</time>` : next && l.index === next.index ? "<em>Next</em>" : "";
      return `<li${cls ? ` class="${cls}"` : ""}><div class="badge"><svg viewBox="0 0 24 24">${LEVEL_ICON[l.index] || ""}</svg></div><b>${esc(l.name)}</b><span>${n(l.at)}</span>${when}</li>`;
    }).join("");
    const top = d.speedLevels.filter((s) => s.days > 0).at(-1);
    const speed = d.speedLevels.map((s) => {
      const got = s.days > 0;
      const best = s.best ?? d.fastestDay?.wpm;
      const sub = got ? `${s.wpm} wpm, ${plural(s.days, "day", "days")}` : best != null ? `Best so far ${best} wpm` : `${s.wpm} wpm`;
      const cls = [got ? "got" : "", s === top ? "now" : ""].filter(Boolean).join(" ");
      return `<li${cls ? ` class="${cls}"` : ""}><div class="badge"><svg viewBox="0 0 26 24">${SPEED_ICON[s.times] || ""}</svg></div><div><b>${esc(s.name)}<i>${s.times}x typing</i></b><span class="num">${sub}</span></div></li>`;
    }).join("");
    return `<section class="levels">
      <h2 class="k">Words</h2>
      <ol class="track" data-fill="${now >= 1 ? ((now - 1) / 7) * 100 : 0}">${track}</ol>
      <div class="here">
        <div class="here-bar"><div class="here-fill" data-w="${progress(d) * 100}"></div></div>
        <div class="here-ends num"><span><strong>${count("so-far", d.total)} words</strong> so far</span><span>${pace(d)}</span></div>
      </div>
      <div class="speed">
        <h2 class="k">Speed, earned by the day</h2>
        <ul>${speed}</ul>
      </div>
    </section>`;
  }

  function progress(d) {
    const next = d.level.next;
    if (!next) return 1;
    const from = d.level.index >= 1 ? d.level.at : 0;
    return Math.min(1, Math.max(0, (d.total - from) / (next.at - from)));
  }

  function view(d) {
    return `<article class="page words">
      <header class="w-head">
        <div class="kicker">${LINES}<span>Words</span></div>
        <span class="w-since">since ${short(d.since)} ${day(d.since).getUTCFullYear()}</span>
      </header>
      <section class="w-hero">${gauge(d)}${saved(d)}</section>
      ${tiles(d)}
      ${rhythm(d)}
      ${levels(d)}
    </article>`;
  }

  function renderDetail(detail) {
    const d = state.data;
    if (!d) {
      state.drawn = null;
      detail.innerHTML = `<div class="blank">${esc(state.error || "Loading…")}</div>`;
      return;
    }
    if (!d.history) {
      state.drawn = d;
      state.play = false;
      detail.innerHTML = '<div class="blank">History is off, so nothing is counted.</div>';
      return;
    }
    if (!d.since || !d.total) {
      detail.innerHTML = '<div class="blank">No dictations yet.</div>';
      return;
    }
    const page = $(".page.words", detail);
    // A library reload redraws the detail pane; the Words view only redraws
    // when its own numbers changed.
    if (page && state.drawn && JSON.stringify(state.drawn) === JSON.stringify(d)) { state.drawn = d; return; }
    const update = !!page && !state.play;
    const play = state.play && !still();
    const scroll = detail.scrollTop;
    detail.innerHTML = view(d);
    state.drawn = d;
    state.play = false;
    const root = $(".page.words", detail);
    // Widths go through the CSSOM: the webview drops style attributes.
    root.querySelectorAll("[data-w]").forEach((el) => { el.style.width = `${el.dataset.w}%`; });
    const track = $(".track", root);
    track.style.setProperty("--fill", `${track.dataset.fill}%`);
    const svg = $(".rhythm", root), read = $(".r-read", root), rest = read.textContent;
    svg.addEventListener("mouseover", (e) => { const t = e.target.getAttribute?.("data-tip"); if (t) read.textContent = t; });
    svg.addEventListener("mouseleave", () => { read.textContent = rest; });
    if (update) detail.scrollTop = scroll;
    else detail.scrollTop = 0;

    const w = Math.min(d.wpm ?? 0, DIAL_MAX);
    const fill = $(".g-fill", root), needle = $(".g-needle", root);
    if (play) {
      root.classList.add("play");
      fill?.animate([{ strokeDashoffset: w }, { strokeDashoffset: 0 }], { duration: PLAY_MS, easing: EASE, fill: "backwards" });
      needle?.animate([{ transform: `rotate(${-w * 1.5}deg)` }, { transform: "rotate(0deg)" }], { duration: PLAY_MS, easing: EASE, fill: "backwards" });
      tick(root, (el) => +(el.dataset.from ?? 0));
    } else if (update && !still()) {
      const was = state.shownWpm == null ? w : Math.min(state.shownWpm, DIAL_MAX);
      if (was !== w) {
        fill?.animate([{ strokeDashoffset: w - was }, { strokeDashoffset: 0 }], { duration: 600, easing: EASE });
        needle?.animate([{ transform: `rotate(${(was - w) * 1.5}deg)` }, { transform: "rotate(0deg)" }], { duration: 600, easing: EASE });
      }
      tick(root, (el) => state.shown[el.dataset.n] ?? +el.dataset.to, 600);
    }
    state.shownWpm = d.wpm;
    root.querySelectorAll("[data-n]").forEach((el) => { state.shown[el.dataset.n] = +el.dataset.to; });
  }

  let raf = 0;
  function tick(root, from, dur = PLAY_MS) {
    cancelAnimationFrame(raf);
    const els = [...root.querySelectorAll("[data-n]")].map((el) => ({ el, from: from(el), to: +el.dataset.to, dec: +el.dataset.dec }))
      .filter((x) => x.from !== x.to);
    if (!els.length) return;
    const t0 = performance.now();
    const step = (now) => {
      const u = Math.min(1, Math.max(0, (now - t0) / dur)), e = 1 - Math.pow(1 - u, 4);
      for (const x of els) x.el.textContent = n(x.from + (x.to - x.from) * e, x.dec);
      if (u < 1) raf = requestAnimationFrame(step);
    };
    step(t0);
  }

  function reload() {
    reloadPending = true;
    if (!loading) loading = Promise.resolve().then(async () => {
      try {
        while (reloadPending) {
          reloadPending = false;
          await refresh();
        }
      } finally { loading = null; }
    });
    return loading;
  }

  async function refresh() {
    try {
      const d = await tauri.core.invoke("library_words");
      state.data = d;
      state.error = "";
      const sub = $("#words-row .row-sub");
      if (sub) sub.textContent = rowSub();
      if (L.current() === WORDS) renderDetail($("#detail"));
    } catch (error) {
      state.error = `Words could not load: ${error}`;
      if (L.current() === WORDS && !state.data) renderDetail($("#detail"));
      else L.toast(state.error);
    }
  }

  window.seeWords = {
    row, renderDetail, reload,
    open() { state.play = true; reload(); },
  };
  tauri.event.listen("library-changed", reload);
  window.addEventListener("focus", reload);
  reload();
})();
