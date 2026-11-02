//! The Library's Dictionary section: every term in `vocabulary.md` and
//! `learned.md` as one row, how each has come out in saved dictations and
//! meetings, and the edits the files allow. The files stay the source of
//! truth; the section reads them fresh on every change and writes through the
//! ledger thread.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::history::History;
use crate::ledger::{Learning, Ledger};

use crate::shadow::Text;
use crate::vocabulary::{same, Document, Edit, Files, Line};

pub struct Dictionary {
    ledger: Ledger,

    /// Fixed at launch, the moment the ledger fixes the one it writes.
    vocabulary: PathBuf,
}

impl Dictionary {
    pub fn new(ledger: Ledger, _history: History) -> Dictionary {
        Dictionary {
            ledger,

            vocabulary: crate::boost::Lexicon::default_path(),
        }
    }

    fn files(&self) -> Files {
        Files::beside(&self.vocabulary)
    }

    /// The stamp is taken before the reads, so a change that lands while
    /// they run makes the next poll differ instead of hiding in this one.
    fn snapshot(&self) -> Result<Snapshot, String> {
        let files = self.files();
        let sources = self.sources();
        crate::library::request(&sources);
        let mut snapshot = snapshot(
            &display(&files.vocabulary)?,
            &display(&files.learned)?,
            &self.ledger.learning(),
        );
        snapshot.stamp = stamp(&sources);
        Ok(snapshot)
    }

    fn sources(&self) -> Vec<PathBuf> {
        let files = self.files();
        vec![files.vocabulary, files.learned]
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

fn stamp(sources: &[PathBuf]) -> String {
    let mut hasher = DefaultHasher::new();
    for path in sources {
        path.hash(&mut hasher);
        crate::shadow::stamp(path).hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    stamp: String,
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
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
struct Rule {
    heard: String,
    learned: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
struct Waiting {
    term: String,
    heard: Option<String>,
}

/// One row per term, whichever file and casing it came from. Active lines
/// win over parked ones, and a casing in `learned.md` wins over the one in
/// `vocabulary.md`, the same way the decoder reads them.
fn snapshot(vocabulary: &Document, learned: &Document, learning: &Learning) -> Snapshot {
    let mut words: Vec<Word> = Vec::new();
    let blank = |term: String, weight: f32| Word {
        term,
        parked: false,
        vocabulary: false,
        learned: false,
        weight,
        rules: Vec::new(),
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
        words,
        waiting,
    }
}

#[tauri::command]
pub async fn dictionary_load(state: tauri::State<'_, Dictionary>) -> Result<Snapshot, String> {
    state.snapshot()
}

#[tauri::command]
pub async fn dictionary_stamp(state: tauri::State<'_, Dictionary>) -> Result<String, String> {
    Ok(stamp(&state.sources()))
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
