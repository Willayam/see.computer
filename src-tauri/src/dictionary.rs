//! The Dictionary window: every term in `vocabulary.md` and `learned.md` in
//! one list, how each has come out in past dictations, and the edits the
//! files allow. The files stay the source of truth; the window reads them
//! fresh on every change and writes through the ledger thread.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use tauri::window::{Effect, EffectState, EffectsBuilder};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::history::History;
use crate::ledger::{Learning, Ledger};
use crate::vocabulary::{same, Document, Edit, Files, Line};

const LABEL: &str = "dictionary";

pub struct Dictionary {
    ledger: Ledger,
    history: History,
    /// Fixed at launch, the moment the ledger fixes the one it writes.
    vocabulary: PathBuf,
    corpus: Mutex<Corpus>,
}

impl Dictionary {
    pub fn new(ledger: Ledger, history: History) -> Dictionary {
        Dictionary {
            ledger,
            history,
            vocabulary: crate::boost::Lexicon::default_path(),
            corpus: Mutex::new(Corpus::default()),
        }
    }

    fn files(&self) -> Files {
        Files::beside(&self.vocabulary)
    }

    /// The stamp is taken before the reads, so a change that lands while
    /// they run makes the next poll differ instead of hiding in this one.
    fn snapshot(&self) -> Result<Snapshot, String> {
        let files = self.files();
        let stamp = self.stamp(&files);
        let (vocabulary, learned) = files
            .read()
            .map_err(|error| format!("Unable to read your dictionary: {error}."))?;
        let learning = self.ledger.learning();
        let mut corpus = self.corpus.lock().unwrap_or_else(PoisonError::into_inner);
        corpus.refresh(&self.history);
        let mut snapshot = snapshot(&vocabulary, &learned, &learning, &corpus);
        snapshot.stamp = stamp;
        Ok(snapshot)
    }

    /// Changes whenever anything the window shows could have: either file,
    /// the learner's queue, or a new dictation in history.
    fn stamp(&self, files: &Files) -> String {
        let mut hasher = DefaultHasher::new();
        file_stamp(&files.vocabulary).hash(&mut hasher);
        file_stamp(&files.learned).hash(&mut hasher);
        if let Some((_, newest)) = self.history.days().first() {
            file_stamp(newest).hash(&mut hasher);
        }
        let learning = self.ledger.learning();
        learning.learned.hash(&mut hasher);
        for waiting in &learning.waiting {
            waiting.term.hash(&mut hasher);
        }
        learning.heard.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| Some((meta.modified().ok()?, meta.len())))
}

/// Every dictation's text joined into one haystack, re-read only when a day
/// file changes.
#[derive(Default)]
struct Corpus {
    days: Vec<(PathBuf, Option<(SystemTime, u64)>)>,
    dictations: usize,
    exact: String,
    lower: String,
}

impl Corpus {
    fn refresh(&mut self, history: &History) {
        let days: Vec<_> = history
            .days()
            .into_iter()
            .map(|(date, path)| {
                let stamp = file_stamp(&path);
                ((date, path), stamp)
            })
            .collect();
        let unchanged = days.len() == self.days.len()
            && days
                .iter()
                .zip(&self.days)
                .all(|(((_, path), stamp), (old_path, old_stamp))| {
                    path == old_path && stamp == old_stamp
                });
        if unchanged {
            return;
        }
        let mut exact = String::new();
        self.dictations = 0;
        for ((date, path), _) in &days {
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };
            for entry in crate::history::parse_day(*date, &content) {
                exact.push_str(&entry.text);
                exact.push('\n');
                self.dictations += 1;
            }
        }
        self.lower = exact.to_lowercase();
        self.exact = exact;
        self.days = days
            .into_iter()
            .map(|((_, path), stamp)| (path, stamp))
            .collect();
    }

    fn count(&self, needle: &str, case_sensitive: bool) -> usize {
        if case_sensitive {
            crate::boost::occurrences(&self.exact, needle, true)
        } else {
            crate::boost::occurrences(&self.lower, &needle.to_lowercase(), true)
        }
    }
}

#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct Snapshot {
    stamp: String,
    dictations: usize,
    words: Vec<Word>,
    waiting: Vec<Waiting>,
}

#[derive(Debug, PartialEq, serde::Serialize)]
struct Word {
    term: String,
    parked: bool,
    vocabulary: bool,
    learned: bool,
    weight: f32,
    rules: Vec<Rule>,
    right: usize,
    misheard: usize,
}

#[derive(Debug, PartialEq, serde::Serialize)]
struct Rule {
    heard: String,
    learned: bool,
}

#[derive(Debug, PartialEq, serde::Serialize)]
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
    for (in_learned, document) in [(false, vocabulary), (true, learned)] {
        for line in document.lines() {
            let Line::Active(entry) = line else { continue };
            let index = match words.iter().position(|w| same(&w.term, &entry.term)) {
                Some(index) => index,
                None => {
                    words.push(Word {
                        term: entry.term.clone(),
                        parked: false,
                        vocabulary: false,
                        learned: false,
                        weight: entry.weight,
                        rules: Vec::new(),
                        right: 0,
                        misheard: 0,
                    });
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
            None => words.push(Word {
                term: entry.term,
                parked: true,
                vocabulary: true,
                learned: false,
                weight: entry.weight,
                rules: entry
                    .heard
                    .map(|heard| Rule {
                        heard,
                        learned: false,
                    })
                    .into_iter()
                    .collect(),
                right: 0,
                misheard: 0,
            }),
        }
    }
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
        word.right = corpus.count(&word.term, true);
        let capitalized = capitalize(&word.term);
        let opening = if capitalized == word.term {
            0
        } else {
            corpus.count(&capitalized, true)
        };
        let other_casing = corpus
            .count(&word.term, false)
            .saturating_sub(word.right + opening);
        // A heard form inside the term itself ("md" in CLAUDE.md) or inside
        // a longer heard form ("os" in "work os") is not counted again there.
        let mut containers = vec![word.term.to_lowercase()];
        containers.extend(wrong.iter().cloned());
        word.misheard = other_casing
            + wrong
                .iter()
                .map(|form| {
                    let inside: usize = containers
                        .iter()
                        .filter(|outer| outer.len() > form.len())
                        .map(|outer| {
                            crate::boost::occurrences(outer, form, true)
                                * corpus.count(outer, false)
                        })
                        .sum();
                    corpus.count(form, false).saturating_sub(inside)
                })
                .sum::<usize>();
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
        dictations: corpus.dictations,
        words,
        waiting,
    }
}

/// How a term looks opening a sentence, which is not a mishearing.
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
    Ok(state.stamp(&state.files()))
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

/// Opens the window, or brings it forward. The app runs as an accessory with
/// no Dock icon, which cannot own a key window the user can Cmd-Tab to, so it
/// becomes a regular app while the window is open and goes back when it
/// closes. Main thread only.
pub fn open(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        activate();
        return;
    }
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("dictionary.html".into()))
        .title("Dictionary")
        .inner_size(780.0, 540.0)
        .min_inner_size(600.0, 400.0)
        .center()
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .transparent(true)
        .effects(
            EffectsBuilder::new()
                .effect(Effect::Sidebar)
                .state(EffectState::FollowsWindowActiveState)
                .build(),
        )
        .build();
    match built {
        Ok(window) => {
            let closing = app.clone();
            window.on_window_event(move |event| {
                if matches!(event, WindowEvent::Destroyed) {
                    let _ = closing.set_activation_policy(tauri::ActivationPolicy::Accessory);
                    // The next dictation pastes into the frontmost app, so
                    // that has to be the app the user came from, not us.
                    deactivate();
                }
            });
            let _ = window.set_focus();
            activate();
        }
        Err(error) => {
            eprintln!("could not open the dictionary window: {error}");
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
        }
    }
}

fn shared_app() -> Option<&'static objc2::runtime::AnyObject> {
    use objc2::msg_send;
    let class = objc2::runtime::AnyClass::get(c"NSApplication")?;
    unsafe { msg_send![class, sharedApplication] }
}

/// macOS 14's cooperative activation grants this after a click in the menu.
fn activate() {
    if let Some(app) = shared_app() {
        unsafe { objc2::msg_send![app, activate] }
    }
}

/// Hands the focus back to the app the user came from.
fn deactivate() {
    if let Some(app) = shared_app() {
        unsafe { objc2::msg_send![app, deactivate] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(texts: &[&str]) -> Corpus {
        let exact = texts.join("\n");
        Corpus {
            days: Vec::new(),
            dictations: texts.len(),
            lower: exact.to_lowercase(),
            exact,
        }
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
        assert!(row("EAS").parked);
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
}
