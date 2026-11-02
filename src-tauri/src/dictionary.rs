//! The Library's Dictionary section: every term in `vocabulary.md` and
//! `learned.md` as one row, how each has come out in saved dictations and
//! meetings, and the edits the files allow. The files stay the source of
//! truth; the section reads them fresh on every change and writes through the
//! ledger thread.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::history::History;
use crate::ledger::{Learning, Ledger};
use crate::library::Spoken;
use crate::shadow::Text;
use crate::vocabulary::{same, Document, Edit, Files, Line};

/// How many transcript lines a term's detail lists.
const RECENT: usize = 6;

pub struct Dictionary {
    ledger: Ledger,
    history: History,
    /// Fixed at launch, the moment the ledger fixes the one it writes.
    vocabulary: PathBuf,
    memo: Mutex<Option<Snapshot>>,
}

impl Dictionary {
    pub fn new(ledger: Ledger, history: History) -> Dictionary {
        Dictionary {
            ledger,
            history,
            vocabulary: crate::boost::Lexicon::default_path(),
            memo: Mutex::new(None),
        }
    }

    fn files(&self) -> Files {
        Files::beside(&self.vocabulary)
    }

    /// The stamp is taken before the reads, so a change that lands while
    /// they run makes the next poll differ instead of hiding in this one.
    fn snapshot(&self) -> Result<Snapshot, String> {
        let files = self.files();
        let root = self.history.root();
        let mut sources = crate::library::spoken_sources(&root);
        sources.extend([files.vocabulary.clone(), files.learned.clone()]);
        crate::library::request(&sources);
        let stamp = stamp(&sources, &self.ledger.learning());
        memoized(&self.memo, stamp, || {
            let vocabulary = display(&files.vocabulary)?;
            let learned = display(&files.learned)?;
            let learning = self.ledger.learning();
            let corpus = Corpus::new(crate::library::spoken(&root));
            let mut snapshot = snapshot(&vocabulary, &learned, &learning, &corpus);
            snapshot.in_icloud = crate::library::in_icloud(&sources);
            Ok(snapshot)
        })
    }

    fn sources(&self) -> Vec<PathBuf> {
        let files = self.files();
        let mut sources = crate::library::spoken_sources(&self.history.root());
        sources.extend([files.vocabulary, files.learned]);
        sources
    }
}

fn display(path: &Path) -> Result<Document, String> {
    match crate::shadow::read(path) {
        Text::Current(text) | Text::Stale(text) => Ok(Document::parse(&text)),
        Text::Waiting => Ok(Document::default()),
        Text::Missing if path.exists() => Err(format!(
            "Unable to read your dictionary: {}.",
            path.display()
        )),
        Text::Missing => Ok(Document::default()),
    }
}

fn memoized(
    memo: &Mutex<Option<Snapshot>>,
    stamp: String,
    build: impl FnOnce() -> Result<Snapshot, String>,
) -> Result<Snapshot, String> {
    let mut memo = memo
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(done) = memo.as_ref().filter(|done| done.stamp == stamp) {
        return Ok(done.clone());
    }
    let mut fresh = build()?;
    fresh.stamp = stamp;
    *memo = Some(fresh.clone());
    Ok(fresh)
}

fn stamp(sources: &[PathBuf], learning: &Learning) -> String {
    let mut hasher = DefaultHasher::new();
    for path in sources {
        path.hash(&mut hasher);
        crate::shadow::stamp(path).hash(&mut hasher);
    }
    learning.learned.hash(&mut hasher);
    for waiting in &learning.waiting {
        waiting.term.hash(&mut hasher);
    }
    learning.heard.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Every dictation and meeting line, newest first, joined into one haystack
/// for the totals and kept as lines for each term's recent list.
struct Corpus {
    lines: Vec<Spoken>,
    exact: String,
    lower: String,
}

impl Corpus {
    fn new(lines: Vec<Spoken>) -> Corpus {
        let exact = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Corpus {
            lower: exact.to_lowercase(),
            exact,
            lines,
        }
    }
}

/// The spellings that count for and against one term.
struct Forms {
    term: String,
    /// How the term looks opening a sentence, which is not a mishearing.
    capitalized: String,
    /// Lowercased heard forms: its rules' and what the user corrected.
    wrong: Vec<String>,
}

impl Forms {
    /// How often `exact` holds the term as written and how often a misheard
    /// form. `lower` is `exact` lowercased, passed in so a haystack is
    /// lowercased once however many terms are counted in it.
    fn tally(&self, exact: &str, lower: &str) -> (usize, usize) {
        let count =
            |haystack: &str, needle: &str| crate::boost::occurrences(haystack, needle, true);
        let right = count(exact, &self.term);
        let opening = if self.capitalized == self.term {
            0
        } else {
            count(exact, &self.capitalized)
        };
        let other_casing = count(lower, &self.term.to_lowercase()).saturating_sub(right + opening);
        // A heard form inside the term itself ("md" in CLAUDE.md) or inside
        // a longer heard form ("os" in "work os") is not counted again there.
        let mut containers = vec![self.term.to_lowercase()];
        containers.extend(self.wrong.iter().cloned());
        let heard: usize = self
            .wrong
            .iter()
            .map(|form| {
                let inside: usize = containers
                    .iter()
                    .filter(|outer| outer.len() > form.len())
                    .map(|outer| count(outer, form) * count(lower, outer))
                    .sum();
                count(lower, form).saturating_sub(inside)
            })
            .sum();
        (right, other_casing + heard)
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    stamp: String,
    /// Files still downloading from iCloud, whose lines the counts lack.
    in_icloud: usize,
    /// How many dictation and meeting lines the counts cover.
    lines: usize,
    words: Vec<Word>,
    waiting: Vec<Waiting>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Word {
    term: String,
    parked: bool,
    vocabulary: bool,
    learned: bool,
    weight: f32,
    rules: Vec<Rule>,
    right: usize,
    misheard: usize,
    recent: Vec<Recent>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
struct Rule {
    heard: String,
    learned: bool,
}

/// A transcript line the term came out in, as the Library can open it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Recent {
    item: String,
    start_ms: i64,
    at_ms: i64,
    text: String,
    misheard: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
struct Waiting {
    term: String,
    heard: Option<String>,
}

/// One row per term, whichever file and casing it came from. Active lines
/// win over parked ones, and a casing in `learned.md` wins over the one in
/// `vocabulary.md`, the same way the decoder reads them.
fn snapshot(
    vocabulary: &Document,
    learned: &Document,
    learning: &Learning,
    corpus: &Corpus,
) -> Snapshot {
    let mut words: Vec<Word> = Vec::new();
    let blank = |term: String, weight: f32| Word {
        term,
        parked: false,
        vocabulary: false,
        learned: false,
        weight,
        rules: Vec::new(),
        right: 0,
        misheard: 0,
        recent: Vec::new(),
    };
    for (in_learned, document) in [(false, vocabulary), (true, learned)] {
        for line in document.lines() {
            let Line::Active(entry) = line else { continue };
            let index = match words.iter().position(|w| same(&w.term, &entry.term)) {
                Some(index) => index,
                None => {
                    words.push(blank(entry.term.clone(), entry.weight));
                    words.len() - 1
                }
            };
            let word = &mut words[index];
            word.term = entry.term;
            word.weight = word.weight.max(entry.weight);
            if in_learned {
                word.learned = true;
            } else {
                word.vocabulary = true;
            }
            if let Some(heard) = entry.heard {
                if !word
                    .rules
                    .iter()
                    .any(|r| r.heard.eq_ignore_ascii_case(&heard))
                {
                    word.rules.push(Rule {
                        heard,
                        learned: in_learned,
                    });
                }
            }
        }
    }
    for line in vocabulary.lines() {
        let Line::Parked(entry) = line else { continue };
        match words.iter_mut().find(|w| same(&w.term, &entry.term)) {
            Some(word) if word.parked => {
                if let Some(heard) = entry.heard {
                    word.rules.push(Rule {
                        heard,
                        learned: false,
                    });
                }
            }
            Some(_) => {}
            None => {
                let mut word = blank(entry.term, entry.weight);
                word.parked = true;
                word.vocabulary = true;
                word.rules.extend(entry.heard.map(|heard| Rule {
                    heard,
                    learned: false,
                }));
                words.push(word);
            }
        }
    }
    let lowered: Vec<String> = corpus
        .lines
        .iter()
        .map(|line| line.text.to_lowercase())
        .collect();
    for word in &mut words {
        let mut wrong: Vec<String> = word.rules.iter().map(|r| r.heard.to_lowercase()).collect();
        for (term, heard) in &learning.heard {
            if same(term, &word.term) {
                wrong.extend(heard.iter().map(|h| h.to_lowercase()));
            }
        }
        wrong.retain(|form| !same(form, &word.term));
        wrong.sort();
        wrong.dedup();
        let forms = Forms {
            capitalized: capitalize(&word.term),
            term: word.term.clone(),
            wrong,
        };
        (word.right, word.misheard) = forms.tally(&corpus.exact, &corpus.lower);
        if word.right + word.misheard == 0 {
            continue;
        }
        word.recent = corpus
            .lines
            .iter()
            .zip(&lowered)
            .filter_map(|(line, lower)| {
                let (right, misheard) = forms.tally(&line.text, lower);
                (right + misheard > 0).then(|| Recent {
                    item: line.item.clone(),
                    start_ms: line.start_ms,
                    at_ms: line.at_ms,
                    text: line.text.clone(),
                    misheard: misheard > 0,
                })
            })
            .take(RECENT)
            .collect();
    }
    words.sort_by_cached_key(|w| w.term.to_lowercase());
    let waiting = learning
        .waiting
        .iter()
        .filter(|w| {
            !words
                .iter()
                .any(|word| !word.parked && same(&word.term, &w.term))
        })
        .map(|w| Waiting {
            term: w.term.clone(),
            heard: w.heard.first().cloned(),
        })
        .collect();
    Snapshot {
        stamp: String::new(),
        in_icloud: 0,
        lines: corpus.lines.len(),
        words,
        waiting,
    }
}

fn capitalize(term: &str) -> String {
    let mut chars = term.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[tauri::command]
pub async fn dictionary_load(state: tauri::State<'_, Dictionary>) -> Result<Snapshot, String> {
    state.snapshot()
}

#[tauri::command]
pub async fn dictionary_stamp(state: tauri::State<'_, Dictionary>) -> Result<String, String> {
    Ok(stamp(&state.sources(), &state.ledger.learning()))
}

#[tauri::command]
pub async fn dictionary_edit(
    state: tauri::State<'_, Dictionary>,
    edit: Edit,
) -> Result<Snapshot, String> {
    let edit = edit.checked()?;
    state.ledger.edit(edit).recv().map_err(|_| {
        "Unable to save. Quit and reopen see.computer, then try again.".to_owned()
    })??;
    state.snapshot()
}

#[tauri::command]
pub async fn dictionary_reveal(state: tauri::State<'_, Dictionary>) -> Result<(), String> {
    let path = state.files().vocabulary;
    let mut open = std::process::Command::new("open");
    if path.exists() {
        open.arg("-R").arg(&path);
    } else if let Some(folder) = path.parent() {
        open.arg(folder);
    }
    open.spawn()
        .map(drop)
        .map_err(|error| format!("Unable to open Finder: {error}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines newest first, each its own dictation a minute apart.
    fn corpus(texts: &[&str]) -> Corpus {
        Corpus::new(
            texts
                .iter()
                .enumerate()
                .map(|(index, text)| Spoken {
                    item: format!("dictation:2026-10-{:02}", 15 - index),
                    start_ms: 1_000_000 - index as i64 * 60_000,
                    at_ms: 1_000_000 - index as i64 * 60_000,
                    text: (*text).to_owned(),
                })
                .collect(),
        )
    }

    fn snapshot_of(vocabulary: &str, history: &[&str]) -> Snapshot {
        snapshot(
            &Document::parse(vocabulary),
            &Document::default(),
            &Learning::default(),
            &corpus(history),
        )
    }

    #[test]
    fn unchanged_snapshot_reuses_its_counts() {
        let memo = Mutex::new(None);
        let first = memoized(&memo, "same".into(), || {
            Ok(snapshot_of("- Tauri\n", &["Tauri builds"]))
        })
        .unwrap();
        let second = memoized(&memo, "same".into(), || {
            panic!("unchanged snapshot recomputed")
        })
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(second.words[0].right, 1);
        let changed = memoized(&memo, "changed".into(), || {
            Ok(snapshot_of("- Tauri\n", &["Tauri Tauri"]))
        })
        .unwrap();
        assert_eq!(changed.words[0].right, 2);
    }

    #[test]
    fn a_word_file_that_cannot_be_read_is_an_error() {
        let root = std::env::temp_dir().join(format!("dictionary-word-{}", std::process::id()));
        std::fs::create_dir_all(root.join("vocabulary.md")).unwrap();
        assert!(display(&root.join("vocabulary.md")).is_err());
        assert_eq!(
            display(&root.join("learned.md")).unwrap(),
            Document::default(),
            "a file that is not there yet is an empty list"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stamp_tracks_day_files() {
        let root = std::env::temp_dir().join(format!("dictionary-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("2026-10-07.md");
        std::fs::write(&path, "First").unwrap();
        let sources = vec![path.clone()];
        let learning = Learning::default();
        let before = stamp(&sources, &learning);
        assert_eq!(before, stamp(&sources, &learning));
        std::fs::write(&path, "Second version").unwrap();
        let changed = stamp(&sources, &learning);
        assert_ne!(before, changed);
        crate::shadow::read(&path);
        assert_eq!(
            changed,
            stamp(&sources, &learning),
            "writing the copy does not make the next poll reload"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_row_per_term_with_its_rules_and_history() {
        let vocabulary = Document::parse(
            "# vocabulary\n- poteto\n- potato -> poteto\n- pnpm\t3\n\n# - EAS\n# - pnpm\n",
        );
        let learned = Document::parse("- Poteto\n- superbase -> Supabase\n");
        let learning = Learning {
            heard: vec![("Supabase".into(), vec!["super base".into()])],
            waiting: vec![
                crate::learn::Waiting {
                    term: "WorkOS".into(),
                    heard: vec!["work os".into()],
                    from: Vec::new(),
                },
                crate::learn::Waiting {
                    term: "pnpm".into(),
                    heard: vec!["p npm".into()],
                    from: Vec::new(),
                },
            ],
            ..Learning::default()
        };
        let history = corpus(&[
            "Poteto mode is on. I said potato again.",
            "pnpm install, PNPM build. Pnpm opens a sentence.",
            "Supabase and super base and superbase.",
        ]);
        let snapshot = snapshot(&vocabulary, &learned, &learning, &history);
        let row = |term: &str| snapshot.words.iter().find(|w| w.term == term).unwrap();
        assert_eq!(
            snapshot
                .words
                .iter()
                .map(|w| w.term.as_str())
                .collect::<Vec<_>>(),
            ["EAS", "pnpm", "Poteto", "Supabase"]
        );
        let poteto = row("Poteto");
        assert!(poteto.vocabulary && poteto.learned && !poteto.parked);
        assert_eq!(
            poteto.rules,
            [Rule {
                heard: "potato".into(),
                learned: false
            }]
        );
        assert_eq!((poteto.right, poteto.misheard), (1, 1));
        assert_eq!(
            poteto.recent,
            [Recent {
                item: "dictation:2026-10-15".into(),
                start_ms: 1_000_000,
                at_ms: 1_000_000,
                text: "Poteto mode is on. I said potato again.".into(),
                misheard: true,
            }]
        );
        let pnpm = row("pnpm");
        assert!(!pnpm.parked, "an active line wins over a parked copy");
        assert_eq!(pnpm.weight, 3.0);
        assert_eq!(
            (pnpm.right, pnpm.misheard),
            (1, 1),
            "PNPM is misheard, Pnpm only opens a sentence"
        );
        let supabase = row("Supabase");
        assert_eq!((supabase.right, supabase.misheard), (1, 2));
        assert_eq!(supabase.recent[0].item, "dictation:2026-10-13");
        assert!(row("EAS").parked);
        assert!(row("EAS").recent.is_empty());
        let claude = snapshot_of("- md -> CLAUDE.md\n", &["Read CLAUDE.md, then claude md."]);
        assert_eq!(
            (claude.words[0].right, claude.words[0].misheard),
            (1, 1),
            "the md inside CLAUDE.md is not a mishearing"
        );
        assert_eq!(
            snapshot.waiting,
            [Waiting {
                term: "WorkOS".into(),
                heard: Some("work os".into())
            }],
            "a waiting word already in the list is not offered again"
        );
    }

    #[test]
    fn recent_lines_are_the_newest_first_and_capped() {
        let texts: Vec<String> = (0..10).map(|n| format!("Tauri build {n}")).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let snapshot = snapshot_of("- Tauri\n", &texts);
        let recent = &snapshot.words[0].recent;
        assert_eq!(recent.len(), RECENT);
        assert_eq!(recent[0].text, "Tauri build 0");
        assert!(recent.iter().all(|line| !line.misheard));
    }
}
