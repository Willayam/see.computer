//! The Dictionary reads active pairs from its two word files and sends edits
//! through the ledger thread so learning and page edits share one writer.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::ledger::Ledger;
use crate::shadow::Text;
use crate::vocabulary::{same, Document, Edit, Files, Line};

pub struct Dictionary {
    ledger: Ledger,

    /// Fixed at launch, the moment the ledger fixes the one it writes.
    vocabulary: PathBuf,
}

impl Dictionary {
    pub fn new(ledger: Ledger) -> Dictionary {
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
        let taken = stamp(&sources);
        crate::library::request(&sources);
        let mut snapshot = snapshot(&display(&files.vocabulary)?, &display(&files.learned)?);
        snapshot.stamp = taken;
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
pub struct Snapshot {
    stamp: String,
    rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
struct Row {
    term: String,
    heard: Option<String>,
    learned: bool,
}

fn snapshot(vocabulary: &Document, learned: &Document) -> Snapshot {
    let mut rows: Vec<Row> = Vec::new();
    for (in_learned, document) in [(false, vocabulary), (true, learned)] {
        for line in document.lines() {
            let Line::Active(entry) = line else { continue };
            let found = rows.iter_mut().find(|row| {
                same(&row.term, &entry.term)
                    && match (&row.heard, &entry.heard) {
                        (None, None) => true,
                        (Some(a), Some(b)) => same(a, b),
                        _ => false,
                    }
            });
            if let Some(row) = found {
                if in_learned {
                    row.term = entry.term;
                    row.heard = entry.heard;
                    row.learned = true;
                }
            } else {
                rows.push(Row {
                    term: entry.term,
                    heard: entry.heard,
                    learned: in_learned,
                });
            }
        }
    }
    rows.sort_by_cached_key(|r| {
        (
            r.term.to_lowercase(),
            r.heard.as_ref().map(|h| h.to_lowercase()),
        )
    });
    Snapshot {
        stamp: String::new(),
        rows,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_is_flat_sorted_and_learned_spelling_wins() {
        let snap = snapshot(
            &Document::parse("# - Veltrix\n- Zentrix\n- zentrixx -> Zentrix\n- Quorvo\n"),
            &Document::parse("- ZENTRIXX -> ZENTRIX\n"),
        );
        assert_eq!(
            snap.rows,
            [
                Row {
                    term: "Quorvo".into(),
                    heard: None,
                    learned: false
                },
                Row {
                    term: "Zentrix".into(),
                    heard: None,
                    learned: false
                },
                Row {
                    term: "ZENTRIX".into(),
                    heard: Some("ZENTRIXX".into()),
                    learned: true
                },
            ]
        );
    }
}
