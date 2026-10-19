# Dictionary and accuracy

How see.computer gets custom words right, what was wrong with it, what the
learning loop added, and what to build next. Numbers come from one user's
machine on 2026-10-14: 2,688 dictations from 2026-08-26 on, and a 104-clip
synthetic corpus. The scripts that produced them are listed under
[Measuring](#measuring).

## How a dictation becomes text

1. The engine decodes with Parakeet TDT 0.6b v3 (`engine/parakeet.rs`). The
   terms in `vocabulary.md` are compiled into a prefix tree over the model's
   own tokens, and each greedy step adds a bonus to tokens that continue a
   term, but only when the token is within a margin of the model's own best
   choice (`boost.rs`, margin 3, scale 8).
2. Two passes tidy the text: hesitation sounds are dropped (`filler.rs`) and
   spoken numbers become figures (`numbers.rs`).
3. The text is pasted with Cmd+V, unless Accessibility says nothing editable
   has focus (`paste.rs`, `target.rs`).
4. Before this work, the final text was appended to `YYYY-MM-DD.md` and
   nothing else was kept: no audio, no app, no record of what the user did to
   the words afterwards (`history.rs`).

## What was wrong

Measured, in order of how much each costs the user.

**Most vocabulary terms never land.** Only 17 of the 63 active terms appear
anywhere in 2,688 dictations. Nine more appear only misspelled, and four of
those are misspelled every time: a project name heard as "potato" 53 times
out of 53, "Incarnate", "post hoc" and "Shad CN". "Claude MD" never once
came out as `CLAUDE.md`. Boosting cannot reach these. The margin gate
(`boost.rs`, `bias`) only helps a term whose first token is already a near
miss, and when the model is confident in an ordinary word, the term is never
in the running.

**Boosting is close to a coin flip on the clips it changes.** On the
synthetic corpus, margin 3 changed 24 of 104 outputs: 13 improved and 11 got
worse, while term recall rose from 19/108 to 35/108. The damage is
collateral. A term's opening token wins a step and the term is then
abandoned, leaving "Crack the" for "Track the", "Install the drafts", or
"Frame One". Margin 1, recommended after a 2026-09-03 sweep and never
merged, changes only 5 clips and recovers 24/108.

| margin | changed clips | better | worse | terms | WER |
|---|---|---|---|---|---|
| off | | | | 19/108 | 19.55% |
| 1 | 5 | 3 | 2 | 24/108 | 19.34% |
| 2 | 15 | 7 | 8 | 29/108 | 19.44% |
| 3 (default) | 24 | 13 | 11 | 35/108 | 19.24% |
| 4 | | | | 37/108 | 21.19% |
| 5 | | | | 40/108 | 33.44% |

**Nothing learned.** The vocabulary changed only when the user edited a file.
The parse comment in `boost.rs` promised a correction capture that did not
exist. Without audio there was no way to replay a bad dictation, so there was
no eval beyond two fixture files.

**Common words the vocabulary cannot touch.** "skill" came out as "scale" 11
times against 53 correct. Both are dictionary words, so no list of custom
terms can fix this without context.

**Single-letter pastes from near-silent captures.** 24 dictations were just
"A", 10 just "H", 2 just "F".

**Stray letters.** 16 to 18 isolated lowercase letters per 10,000 words
("s sleep", "so this is fine c but"), steady since boosting shipped. The rate
per dictation doubled, but only because dictations got longer.

**Hidden history.** An iCloud conflict copy, `2026-10-13 2.md`, held 20 of
that day's 22 dictations. `history.rs` skips any file whose name is not a
date, so the menu's recents never showed them.

**Read-back has to work in web apps.** 93% of dictations land in web-based
UIs: 61% in Electron or Chromium (T3 Code, Grok, Discord, Chrome) and 32% in
WebKit (Conductor). Electron exposes no Accessibility tree until a client asks
for one, so `target.rs` reports "unknown" there.

**Read-back never caught a fix in T3 Code.** On 2026-10-16 every T3 Code
watch ended "unchanged" three to four seconds after its paste. A paste leaves
a space after the words, and a TipTap composer with text in it reads back as
that text plus the one space. `region()` looked for the text after the
dictation from the start of the field, found the space after "I", and cut
the snapshot to one word. Three polls in a row then failed to resemble the
dictation, and the watch ended before the user had typed anything.

**Nothing showed that a fix was seen.** A read-back sighting weighs 1, so it
only became a "seen once" entry in the menu and the Dictionary, and nothing
on screen said so.

**The Library opened blank.** With the disk nearly full, macOS had moved
every file the Library reads to iCloud, 204 of them on 2026-10-16: day files,
transcripts, meeting files and share records. Reading one waits for its
download, 1.2 to 1.4 s each, one after another, so `library_items` ran for
minutes and the window showed nothing meanwhile. The Dictionary route waited
for that load before it loaded itself, and then opened on the first word
alphabetically, "AGENTS.md", which had never been dictated: "0 right · 0
misheard · Not seen in your 2,919 saved lines yet". The counts were right.
Codex had 34 right and 3 misheard, Claude 18.

**No eval seed from Wispr Flow.** Its local database keeps audio for only 3
history rows, none of them edited. The personal eval corpus has to come from
see.computer itself.

## The learning loop

```
dictate ─► ledger: id, text, audio, app ─► paste
                                            │
            read-back (AX) or Library fix ◄─┘
                         │
                         ▼
                  Corrected event ─► learner ─► learned.md ─► decoder
                         │                       (terms and     (boost,
                         ▼                        heard -> Term  rewrite)
                    corpus.tsv ─► see-computer eval
```

While a dictation is recording, a background task reads the focused window's
Accessibility text, one line per element. It visits at most 800 elements, 64
levels deep, for 800 ms, and accepts at most 16,000 characters. Web apps nest
their text deep: T3 Code's transcript sits about 27 levels under its window,
and the first version, which stopped at five levels and 96 elements, only
ever read the focused field. On T3 Code the walk reads about 670 elements in
under 100 ms.

The terms kept from that text are names and identifiers: camelCase,
internal punctuation (`claude.ai`), letters mixed with digits, all caps the
word list does not know, and capitalized words the word list does not know.
A capital that opens a sentence does not count, but a word alone on its line
does, since that is a label or a name. Dictionary words, plain lowercase
words, month and day names, and units stay out. A dictionary word
capitalized inside a sentence, as in "the rows in Convex", counts when the
text never writes it in lowercase, and ranks after the rest. Terms are
ranked by how often they appear. A dictation gets at most 32 screen terms,
then at most 8 Daybook terms, with no spelling twice (`context::gather`).
Before this, each dictation was boosted with 62 to 65 terms, among them
"And", "Pro", "dropdown" and "Comparing".

The engine adds those terms at weight 0.5 for this utterance, through the
same dual decode and merge as saved vocabulary. A hunk survives only when it
lands a whole term. The context is cleared after the utterance.
`SEE_COMPUTER_SCREEN_CONTEXT=0` turns the source off.

**Near misses of names on screen.** Boosting cannot reach a name the model
hears as another spelling. With "Larsten" in context, the boosted pass takes
the name's opening token and then leaves it ("Lauriston", "Lawson"), and the
merge rightly keeps the plain pass's "Larston". Weights up to 3 and margins
up to 8 did not change that on the user's three recordings. After decoding,
a capitalized word the word list does not know, at least five letters long,
is written as a screen term when it is one changed or swapped letter from
exactly one of them and shares its first two letters
(`context::near_misses`). Saved vocabulary words are left alone. A letter
added or dropped is not enough: run over the user's 2,926 saved lines with
every vocabulary term treated as on screen, allowing those also turned the
Swedish "Fridag" and "Fridas" into "Frida". With one changed or swapped
letter, 4 lines changed, all of them misspellings of vocabulary names
("Larston", "Hormozy", "Fridd").

Another background task reads Daybook's `daybook.db` with SQLite `mode=ro`.
It reads only non-private descriptions from the last two hours and refreshes
its in-memory term cache every three minutes. A missing or changed database
leaves the cache empty. The default path is
`~/Development/automation-project/data/daybook.db`; `SEE_COMPUTER_DAYBOOK_DB`
overrides it and `SEE_COMPUTER_DAYBOOK_CONTEXT=0` turns this source off. The
decoder takes the cached terms at the same low weight. The ledger records
each term used, its source, and whether it appeared in the result.

**The ledger** (`ledger.rs`) gives every dictation an id and appends events to
`~/Library/Application Support/see.computer/ledger/events.jsonl`. The events
record what was decoded and the audio path, where the words landed (app,
bundle id, focus), what the user corrected them to, and what was learned or
forgotten. Audio is a 16 kHz mono wav per dictation. The newest 500
uncorrected files are kept, and corrected ones are kept forever, because
they are the eval corpus. The "Save Dictation History" toggle governs all of
it.

**Read-back** (`readback.rs`) keeps the text field the paste went into,
taken from the same Accessibility query that decided to paste. It re-reads
that field every second for up to 90 seconds and keeps the last snapshot that
still resembles the dictation, so a fix followed by Enter is caught before the
field clears. Text typed before or after the dictated words is trimmed away,
and an edit that grows or shrinks the dictation beyond a few words is treated
as a rewrite and not recorded. In Electron apps it sets
`AXManualAccessibility`, the same switch screen readers use, so the field
becomes readable. It stops when "Save Dictation History" is off. The text
before and after the dictation are its anchors. An anchor that is only
whitespace is ignored, and one that reached the edge of the field is matched
at that edge. With `SEE_COMPUTER_READBACK_LOG` set, a watch that gives up
logs why: `lost no-anchor`, `lost unlike`, `lost too-long`, or the
Accessibility error.

**Seeing a fix.** When read-back records a correction with a spelling the
learner can use, the pill shows a card such as `Larston → Larsten · Seen
once`, or `Learned` when this fix taught the word. Review opens the word in
the Dictionary, where Learn now and Ignore sit at the top of the list. The
card waits until no dictation is on screen and closes after six seconds
unless the pointer rests on it. It is a nonactivating panel, so it never
takes focus from the app being typed into. A correction that teaches
nothing, such as a grammar fix, still goes to the ledger and the eval corpus
but shows no card.

**Fixing a line in the Library** is the explicit path for text that was
already sent or an app that exposes nothing. Every dictation and finished
meeting line in the Library has a fix button. The fixed text replaces the
line in its file (the history thread rewrites day files, since it is their
only writer), and the ledger records the fix as a manual correction under the
line's own dictation. A line the ledger never saw, such as a dictation from
before the ledger or a meeting, gets a dictation entry of its own first. Fix
Last Dictation… in the menu opens the newest dictation this way.

**The learner** (`learn.rs`) aligns each correction against what was pasted
and keeps only swaps the model could not have written unprompted: a capital
mid-sentence, camelCase, all caps, internal punctuation, letters mixed with
digits, or a word the system word list does not know (inflections included,
so "sketched" is known). Grammar fixes (to → too), lowered capitals and
numbers never qualify. An offline study of this user's 13,700 Wispr Flow
edits on 2026-08-27 found this test 91% precise once a term has two
sightings and 37% at one; its scripts were not kept, so treat the figures as
the reason for the two-sighting rule rather than as a reproduced result.
Two sightings promote the term. A mishearing seen twice also promotes a
rule, `heard -> Term`, which rewrites what the model heard. A rule is the
only fix for a term the model is confident it heard as an ordinary word, and
it applies to terms already in `vocabulary.md`. Because a rule rewrites every
future occurrence, one whose heard form is made of ordinary words ("potato")
needs three separate dictations. A fix typed into the dialog counts as two
sightings for terms and for rules on unknown words. Learned lines go to
`learned.md` beside `vocabulary.md`, the pill says `Learned "Term"`, and a
term the user forgets never comes back.

**Rules measured.** Eleven rules learned at two sightings from one synthetic
voice's corrections ("superbase -> Supabase", "versal -> Vercel", "work os ->
WorkOS") were applied through the real engine to the other voice, which they
never saw. Its term recall rose from 15/58 to 26/58 and its mean per-clip WER
fell from 28.2% to 24.1%.

**Dual decoding.** The engine runs the encoder once and greedy-decodes its
output twice, with and without the vocabulary bias (`engine/parakeet.rs`,
`boost::merge`, a small addition to the vendored decoder). Where the two
disagree, the boosted words are kept only if they add a whole vocabulary
term the plain words lack. At margin 3 this takes the synthetic corpus from
187 to 182 errors and from 35 to 36 term hits, and costs 10.7 ms per clip at
the median (100.0 to 110.8 ms, release build). The Rust result matches the
offline simulation exactly.

| margin | merge | errors | WER | terms |
|---|---|---|---|---|
| 3 | off | 187 | 19.24% | 35/108 |
| 3 | on | 182 | 18.72% | 36/108 |
| 4 | off | 206 | 21.19% | 37/108 |
| 4 | on | 188 | 19.34% | 39/108 |

Margin 3 stays the default: with the merge it has the fewest errors.

**The menu.** Dictionary shows up to eight learned words (click to forget),
terms seen once (click to learn now), Fix Last Dictation…, and Edit
Dictionary….

**The Dictionary in the Library.** Edit Dictionary… opens the Library window
on its Dictionary destination, beside All, Dictations, Takes and Meetings
(`ui/dictionary.js`, `dictionary.rs`). It lists every term in `vocabulary.md`
and `learned.md` as one row, with the words seen once above them and the
parked ones (`# - Term` lines) below. For each term it shows how often the
exact spelling appears in saved dictations and meetings and how often a
misheard form does: another casing that is not just a sentence opening, a
rule's heard form, or a form the user corrected. It also lists the newest
lines the term came up in, each of which opens in the Library, where it can
be fixed. From there the user adds, renames, deletes, parks and turns terms
back on, edits a term's heard forms, sets its boost (the tab-separated
weight), and learns or ignores a word seen once.

The files stay the source of truth. `vocabulary.rs` reads each file as its
own lines, and an edit re-reads the bytes, changes only the lines that hold
the term it names, and writes the result through a temp file. If a file
changed between that read and the write, the edit is applied again to the new
bytes. An edit to a term that is no longer there is refused and the section
reloads, so a stale Library cannot overwrite a sync from another Mac. Writes
go through the ledger thread, which already appends to `learned.md`, so the
learner and the Library never write it at once. Deleting or renaming a
learned term records it as forgotten, so the learner does not bring the old
spelling back. The section polls a cheap stamp of both files and the
learner's queue, reloads on the Library's own change event, and holds a
reload while the user is mid-edit.

Spellings that differ only in case are one term. Renaming a term rewrites
its lines in both files instead of adding a second entry, and when
`learned.md` holds a new casing of a `vocabulary.md` term, the decoder uses
the learned spelling and points the term's rules at it (`Lexicon::refresh`).

**Opening the Library.** The Dictionary loads on its own, without waiting
for the Library's dictations, takes and meetings, and both panes say they
are loading. It opens on the newest fix waiting for review, or else on the
word that came up most recently, and says where fixes appear when none are
waiting. Before reading, the Library asks iCloud for every evicted file it
needs in one batch through `NSFileManager`, waits up to 3 s, and shows what
is still missing as "Downloading from iCloud". A watcher reloads the Library
as the files arrive, and loads made while it runs do not wait again. Asking
in one batch brought 30 files back in 2.5 s, where reading them took about
0.8 s each.

## What is verified

- Unit and controller tests: 240 pass, one live sharing test is ignored.
  They cover the
  ledger, the learner and its filter, rules, the merge, read-back's text
  matching and trimming, eval scoring, the menu rows, the vocabulary line
  edits and their byte preservation, the per-term counts and recent lines,
  and fixing a day-file entry, a meeting line, and the ledger entry behind
  either.
- The eval numbers above come from the release build on the synthetic corpus.
- The Library, run on 2026-10-15 against copies of the real files through
  `SEE_COMPUTER_DOCUMENTS_DIR`, `SEE_COMPUTER_HISTORY_DIR` and
  `SEE_COMPUTER_LEDGER_DIR`: in the Dictionary section, Learn wrote
  `learned.md` and a `learned` event, Ignore a `forgotten` event, and adding
  then deleting a word left `vocabulary.md` byte-identical. A term's recent
  line opened its dictation in the Library. Fixing "Use potato mode" in a
  dictation changed that one line of its day file and logged a dictation
  entry and a manual correction; fixing a meeting line changed that one line
  of `meeting.md` and logged the same pair at the meeting's start plus the
  line's offset.
- Focus after the Library closes, run on 2026-10-15 with a scratch TextEdit
  document frontmost: with only `[NSApp deactivate]`, the workspace still
  named see.computer frontmost after the close, and the next dictation was
  logged as landing in see.computer and never reached TextEdit. With the
  Library handing activation back to the app it opened over, the workspace
  named TextEdit within half a second, and a dictation replayed from
  `fixtures/en.wav` was pasted into the document.
- On 2026-10-16, the release eval on 104 regenerated synthetic clips with a
  temporary vocabulary copy gave 176 errors in 972 words, 18.11% WER, and
  35/108 term hits. Adding irrelevant screen and Daybook terms gave the same
  per-clip output and summary, with no skipped clips. On the `Convex` clip
  with no saved vocabulary, screen context changed `convex` to `Convex` and
  raised term recall from 0/1 to 1/1, with no word errors in either run.
  Unit tests cover private and old Daybook rows, missing files, the learner
  shape filter, the merge, and the ledger's term-only context event. Clippy
  passes with warnings denied.
- A scratch TextEdit replay was attempted on 2026-10-16 under the live lock.
  The unsigned debug app's event tap repeatedly reset before a dictation
  started. No paste or context event was produced.
- Later on 2026-10-16, after the filter, the deeper walk and the near-miss
  rewrite: 242 tests pass and clippy passes with warnings denied. The 104
  regenerated clips with today's vocabulary copy give 177 errors in 972
  words and 37/108 terms with no context. With real T3 Code screen text and
  real Daybook descriptions as context, the old filter gave 181 errors and
  the new one 177. With every vocabulary term as screen text, the new
  filter gives 177. The `Convex` clips still come out as `Convex` with
  "Convex" on screen and no vocabulary. The user's three "Larsten"
  recordings, which came out "Larston", "Larson" and "Larston", give 3
  errors with no context and 1 with "Larsten" on screen: both "Larston"s
  become "Larsten", and "Larson" stays, two letters away.
- The deeper walk was run read-only against T3 Code while the user worked:
  671 elements and 6,394 characters in 94 ms, against nothing past the
  focused field before.
- Not verified live: screen context during a real dictation into T3 Code.
  The eval applies context from text files, not from a live window.
- On 2026-10-16, after the fixes: 238 tests pass and clippy passes with
  warnings denied. New tests cover a paste that ends its field, with each
  trailing form an editor reported, and the cards a read-back fix sends:
  seen once, then learned, and none for a grammar fix.
- Read-back live on 2026-10-16, in a scratch Electron 44.4.5 app (the
  version T3 Code ships) with a TipTap editor like T3 Code's composer. A
  dictation was replayed from one of the user's "Larston" recordings and
  pasted with Cmd+V. "Larston" was changed to "Larsten" with real key presses
  and sent with Return. Before the fix the log read `lost unlike` three
  seconds after the paste, with no edit made. After it the log read
  `corrected`, and the ledger held "I sent the file to Larsten yesterday."
  seven seconds after the paste. In the same run the card showed, a real
  click on Review opened the Library on Larsten under Seen once, and Learn
  now wrote `- larston -> Larsten` to `learned.md` and said so in a toast.
- T3 Code itself was only read, never typed into. Its composer is a TipTap
  editor whose value is its text plus a trailing space, the case the fix
  handles, and the element read-back holds stayed valid while the user typed.
- The iCloud batch was measured against the real folder: 204 evicted files,
  an answer in 3.2 s with 166 placeholders, and every file back 18 s later.
- Not verified live: read-back in T3 Code and in TextEdit, the card when a
  dictation starts while it is up, and the iCloud placeholders on screen.

## Roadmap

Ranked by expected gain per unit of work. Built items are in this branch.

| # | idea | needs | latency | privacy | expected gain | status |
|---|---|---|---|---|---|---|
| 1 | Correction read-back after paste | AX polling, Electron switch | none on the paste path | reads only the field it pasted into; edited text kept locally | the main input to every other item | built; verified in an Electron TipTap editor |
| 2 | Learner with review queue and rules | ledger events | none | local | rules: held-out term recall 15/58 → 26/58 | built |
| 3 | Personal eval corpus and regression score | ledger audio + corrections | none | audio stays local | turns "feels worse" into a number per change | built (`corpus.tsv`, `see-computer eval`) |
| 4 | Dual decode, keep only boosted hunks that land a term | second greedy pass over the same encoder output | +10.7 ms median per clip | none | errors 187 → 182 at margin 3; margin 4 no longer ends up worse than boost-off (188 vs 190 errors) | built |
| 5 | Fixing lines in the Library, and the Dictionary menu | the Library window, native panel rows | none | none | covers apps read-back cannot see | built |
| 6 | Screen-context terms per dictation | AX text of the focused window read while the user speaks, filtered by the learner's shape test, boosted at low weight for that utterance only | none if read during recording | on-screen text held in memory, never written | names and identifiers visible on screen; also the only lever for "scale" vs "skill" | built |
| 7 | Daybook feed | read `daybook.db` read-only, last two hours of non-private descriptions | none | already local; skip private rows | project and client names, about 6 minutes stale | built |
| 8 | Background miner | nightly job over ledger, history, Daybook, git repos, Wispr's dictionary; optional Haiku pass to judge candidates via the Claude CLI | none | only candidate strings and short contexts leave the machine, and only with Haiku on | fills the review queue before the user corrects anything | later |
| 9 | Per-app term sets | ledger's app per dictation | none | local | fewer collateral pushes by boosting only what that app needs | later, after 6 |
| 10 | Confidence highlighting | token log-probs from the decoder | small | none | points the user at the words to check | later |
| 11 | LLM rewrite on the paste path | network call per dictation | 0.5 to 2 s | every dictation leaves the machine | unclear, and it rewrites what was said | not planned |

Fixes the audit or the review found that this branch does not make:

- Treat a one-letter transcript as nothing heard.
- Read iCloud conflict copies of day files.
- Forget which apps had their Accessibility tree opened when the app quits,
  so a reused process id does not inherit the paste guard.
- Compact the ledger. Its events stay in memory and on disk without bound,
  which is small for a year of dictation but has no ceiling.
- A fix to a word already in `vocabulary.md` gets a card but no Seen once
  entry, because the queue lists only new words.
- The menu's recents still read day files directly, so an evicted day file
  stalls them for about a second.

## Measuring

- `see-computer eval <manifest.tsv> [vocabulary.md]` replays wav files through
  the app's engine and live chunking, and prints per-clip WER, term hits, and a
  pooled summary. Terms are scored against the given vocabulary, so a boost-off
  run stays comparable.
- `scripts/eval-dictionary.sh <binary> <manifest> [margins…]` runs boost off
  and each margin.
- `scripts/make-jargon-corpus.sh <dir>` synthesises the jargon and control
  corpus from `scripts/jargon-sentences.tsv` with `say`, clean and quiet with
  noise.
- `~/Library/Application Support/see.computer/ledger/corpus.tsv` is the
  personal corpus. Run `see-computer eval` on it before and after any change
  to boosting or the vocabulary.
- Env seams: `SEE_COMPUTER_BOOST_MARGIN`, `_SCALE`, `_DEPTH`, `_MERGE=0`,
  `SEE_COMPUTER_VOCABULARY`, `SEE_COMPUTER_LEDGER_DIR`,
  `SEE_COMPUTER_READBACK=0`, `SEE_COMPUTER_READBACK_ELECTRON=0`,
  `SEE_COMPUTER_OPEN_LIBRARY=<route>` (opens the Library at launch on a route
  such as `dictionary` or `edit:dictation:2026-10-15@<ms>`, so a check can
  drive it without clicking the menu bar), `SEE_COMPUTER_DOCUMENTS_DIR` (moves
  the see.computer folder, meetings and takes included, for a check that must
  not touch the real one), and
  `SEE_COMPUTER_READBACK_LOG` (one line per watch with the app and outcome,
  never the text), and `SEE_COMPUTER_PASTE=hold`, which holds the words
  instead of pasting. Use it with the screen locked: the lock screen reports
  an editable field, so a real paste would type into its password box.
- `see-computer eval` exits 2 when any clip was skipped, so a run that
  silently dropped hard clips cannot pass.
- `SEE_COMPUTER_EVAL_SCREEN_TEXT` and `SEE_COMPUTER_EVAL_DAYBOOK_TEXT` name
  text files whose extracted terms are applied to every eval clip. This makes
  the context path measurable without reading a live window or database. The eval ranks and caps them the way a dictation does.

## Privacy

Everything stays on the Mac. The ledger, its audio and the corpus live in
Application Support, not in Documents, which may sync to iCloud.
`learned.md` sits beside `vocabulary.md` and syncs with it. Read-back reads
only the field captured when the paste was decided, or, in an Electron app
that answered nothing then, the field focused a quarter second later. It
reads for at most 90 seconds and stores only the region of the user's own
dictation. Turning off "Save
Dictation History" stops the ledger, the audio, and learning.
The dictation app keeps fetched screen text and Daybook descriptions only in
memory. It writes neither to history nor the ledger. The ledger stores only
context terms used and whether they landed, when history is enabled. The feed
opens the Daybook database read-only and skips rows marked private. An
absent, locked, or changed database never delays dictation.
