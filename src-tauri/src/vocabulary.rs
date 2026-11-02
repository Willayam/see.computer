//! `vocabulary.md` and `learned.md` as lines that can be changed one at a time.
//!
//! Both files are the user's: they edit them by hand, iCloud syncs them from
//! another Mac, and the learner appends to `learned.md`. So an edit never
//! rewrites a file from a model of it. It re-reads the bytes, changes only the
//! lines that hold the term it names, and leaves every other byte alone. A
//! term that is no longer where the edit expected it is refused as stale, so a
//! window showing an old copy cannot clobber a newer one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One boosted term, and the phrase the model hears instead when the line is
/// a `heard -> Term` rule.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub heard: Option<String>,
    pub term: String,
    pub weight: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Active(Entry),
    /// A comment, a blank line, or anything else. Kept byte for byte.
    Other,
}

impl Line {
    /// One term per line. `#` comments and blank lines are skipped, a leading
    /// `-` is allowed so the file reads as a markdown list, and a
    /// tab-separated number overrides the weight. A weight of zero or less
    /// switches the line off.
    pub fn parse(raw: &str) -> Line {
        let line = raw.trim();
        if line.starts_with('#') {
            return Line::Other;
        }
        entry(line.strip_prefix("- ").unwrap_or(line)).map_or(Line::Other, Line::Active)
    }

    fn entry(&self) -> Option<&Entry> {
        match self {
            Line::Active(entry) => Some(entry),
            Line::Other => None,
        }
    }
}

fn entry(body: &str) -> Option<Entry> {
    if body.is_empty() || body.starts_with('#') {
        return None;
    }
    let (body, weight) = match body.split_once('\t') {
        Some((body, weight)) => (body.trim(), weight.trim().parse::<f32>().unwrap_or(1.0)),
        None => (body.trim(), 1.0),
    };
    if !weight.is_finite() || weight <= 0.0 {
        return None;
    }
    let (heard, term) = match body.split_once(" -> ") {
        Some((heard, term)) if !heard.trim().is_empty() && !term.trim().is_empty() => {
            (Some(heard.trim().to_owned()), term.trim())
        }
        _ => (None, body),
    };
    (!term.is_empty()).then(|| Entry {
        heard,
        term: term.to_owned(),
        weight,
    })
}

/// Two spellings name the same term when they differ only in case, so a
/// casing fix replaces a line instead of adding a second one.
pub fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// A file as its lines, each with its own line ending, so writing it back is
/// the original bytes plus exactly the lines an edit touched.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    chunks: Vec<String>,
}

impl Document {
    pub fn parse(text: &str) -> Document {
        Document {
            chunks: text.split_inclusive('\n').map(str::to_owned).collect(),
        }
    }

    pub fn render(&self) -> String {
        self.chunks.concat()
    }

    pub fn lines(&self) -> impl Iterator<Item = Line> + '_ {
        self.chunks.iter().map(|chunk| Line::parse(body(chunk)))
    }

    fn find(&self, wanted: impl Fn(&Line) -> bool) -> Vec<usize> {
        self.lines()
            .enumerate()
            .filter(|(_, line)| wanted(line))
            .map(|(index, _)| index)
            .collect()
    }

    fn active(&self, term: &str) -> Vec<usize> {
        self.find(|line| matches!(line, Line::Active(e) if same(&e.term, term)))
    }

    fn entry_at(&self, index: usize) -> Entry {
        Line::parse(body(&self.chunks[index]))
            .entry()
            .cloned()
            .expect("index came from a search for entries")
    }

    fn rewrite(&mut self, index: usize, line: &Line) {
        let chunk = &self.chunks[index];
        let indent = &chunk[..chunk.len() - chunk.trim_start().len()];
        let bulleted = chunk.trim_start().starts_with("- ");
        let text = render_line(line, bulleted);
        self.chunks[index] = format!("{indent}{text}{}", ending(chunk));
    }

    fn insert(&mut self, line: &Line) {
        let after = self
            .lines()
            .enumerate()
            .filter(|(_, l)| matches!(l, Line::Active(_)))
            .map(|(index, _)| index)
            .last();
        let index = after.map_or(self.chunks.len(), |index| index + 1);
        let newline = self.newline();
        if let Some(previous) = index.checked_sub(1).map(|i| &mut self.chunks[i]) {
            if ending(previous).is_empty() {
                previous.push_str(newline);
            }
        }
        self.chunks
            .insert(index, format!("{}{newline}", render_line(line, true)));
    }

    fn remove(&mut self, indices: &[usize]) {
        for &index in indices.iter().rev() {
            self.chunks.remove(index);
        }
    }

    fn newline(&self) -> &'static str {
        if self.chunks.iter().any(|chunk| chunk.ends_with("\r\n")) {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// Adds an active pair unless this document already holds it.
    pub fn add(&mut self, entry: Entry) -> bool {
        if self.has_pair(&entry.term, entry.heard.as_deref()) {
            return false;
        }
        self.insert(&Line::Active(entry));
        true
    }

    pub fn has_pair(&self, term: &str, heard: Option<&str>) -> bool {
        !self.pair(term, heard).is_empty()
    }

    fn pair(&self, term: &str, heard: Option<&str>) -> Vec<usize> {
        self.find(|line| matches!(line, Line::Active(e) if matches_pair(e, term, heard)))
    }

    pub fn delete(&mut self, term: &str) -> bool {
        let indices = self.active(term);
        self.remove(&indices);
        !indices.is_empty()
    }

    fn delete_pair(&mut self, term: &str, heard: Option<&str>) -> bool {
        let indices = self.pair(term, heard);
        self.remove(&indices);
        !indices.is_empty()
    }

    fn change(&mut self, term: &str, heard: Option<&str>, to: &Entry) -> bool {
        let indices = self.pair(term, heard);
        for &index in &indices {
            let mut entry = to.clone();
            entry.weight = self.entry_at(index).weight;
            let weight = body(&self.chunks[index])
                .split_once('\t')
                .map(|(_, w)| w.to_owned());
            self.rewrite(index, &Line::Active(entry));
            if let Some(weight) = weight {
                let chunk = &self.chunks[index];
                let end = ending(chunk).to_owned();
                let text = body(chunk).split('\t').next().unwrap();
                self.chunks[index] = format!("{text}\t{weight}{end}");
            }
        }
        !indices.is_empty()
    }
}

fn ending(chunk: &str) -> &str {
    if chunk.ends_with("\r\n") {
        "\r\n"
    } else if chunk.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

fn body(chunk: &str) -> &str {
    &chunk[..chunk.len() - ending(chunk).len()]
}

fn render_line(line: &Line, bulleted: bool) -> String {
    let render = |entry: &Entry| {
        let mut text = match &entry.heard {
            Some(heard) => format!("{heard} -> {}", entry.term),
            None => entry.term.clone(),
        };
        if entry.weight != 1.0 {
            text.push('\t');
            text.push_str(&format_weight(entry.weight));
        }
        text
    };
    match line {
        Line::Active(entry) if bulleted => format!("- {}", render(entry)),
        Line::Active(entry) => render(entry),
        Line::Other => String::new(),
    }
}

fn format_weight(weight: f32) -> String {
    if weight.fract() == 0.0 {
        format!("{weight:.0}")
    } else {
        format!("{weight}")
    }
}

pub fn matches_pair(entry: &Entry, term: &str, heard: Option<&str>) -> bool {
    same(&entry.term, term)
        && match (entry.heard.as_deref(), heard) {
            (None, None) => true,
            (Some(a), Some(b)) => same(a, b),
            _ => false,
        }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Edit {
    Add {
        text: String,
    },
    Change {
        term: String,
        heard: Option<String>,
        to: String,
    },
    Delete {
        term: String,
        heard: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    Forgot(String, Option<String>),
    Added(String, Option<String>),
}

fn checked_part(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty()
        || text.contains(['\t', '\n', '\r'])
        || text.starts_with(['#', '-'])
        || text.contains(" -> ")
    {
        return Err(
            "Use a word or replacement without tabs, line breaks, or a leading # or dash.".into(),
        );
    }
    Ok(text.into())
}

fn checked_entry(text: &str) -> Result<Entry, String> {
    let (heard, term) = match text.split_once(" -> ") {
        Some((h, t)) => (Some(checked_part(h)?), checked_part(t)?),
        None => (None, checked_part(text)?),
    };
    Ok(Entry {
        term,
        heard,
        weight: 1.0,
    })
}

impl Edit {
    pub fn checked(self) -> Result<Edit, String> {
        match &self {
            Edit::Add { text } => {
                checked_entry(text)?;
            }
            Edit::Change { term, heard, to } => {
                checked_part(term)?;
                if let Some(h) = heard {
                    checked_part(h)?;
                }
                checked_entry(to)?;
            }
            Edit::Delete { term, heard } => {
                checked_part(term)?;
                if let Some(h) = heard {
                    checked_part(h)?;
                }
            }
        }
        Ok(self)
    }

    pub fn apply(
        &self,
        vocabulary: &mut Document,
        learned: &mut Document,
    ) -> Result<Vec<Note>, String> {
        let exists = |term: &str, heard: Option<&str>| {
            vocabulary.has_pair(term, heard) || learned.has_pair(term, heard)
        };
        let duplicate = || "That row is already in your dictionary.".to_owned();
        let stale = || "That row changed on disk, so the list has been reloaded.".to_owned();
        match self {
            Edit::Add { text } => {
                let entry = checked_entry(text)?;
                if exists(&entry.term, entry.heard.as_deref()) {
                    return Err(duplicate());
                }
                let note = Note::Added(entry.term.clone(), entry.heard.clone());
                vocabulary.add(entry);
                Ok(vec![note])
            }
            Edit::Change { term, heard, to } => {
                if !exists(term, heard.as_deref()) {
                    return Err(stale());
                }
                let entry = checked_entry(to)?;
                let changed = !matches_pair(&entry, term, heard.as_deref());
                if changed && exists(&entry.term, entry.heard.as_deref()) {
                    return Err(duplicate());
                }
                vocabulary.change(term, heard.as_deref(), &entry);
                learned.change(term, heard.as_deref(), &entry);
                let mut notes = Vec::new();
                // A word that gains a replacement is still wanted.
                let kept_word = heard.is_none() && same(term, &entry.term);
                if changed && !kept_word {
                    notes.push(Note::Forgot(term.clone(), heard.clone()));
                }
                notes.push(Note::Added(entry.term, entry.heard));
                Ok(notes)
            }
            Edit::Delete { term, heard } => {
                let a = vocabulary.delete_pair(term, heard.as_deref());
                let b = learned.delete_pair(term, heard.as_deref());
                if !a && !b {
                    return Err(stale());
                }
                Ok(vec![Note::Forgot(term.clone(), heard.clone())])
            }
        }
    }
}

/// The pair of files, read and written as a unit.
pub struct Files {
    pub vocabulary: PathBuf,
    pub learned: PathBuf,
}

impl Files {
    pub fn beside(vocabulary: &Path) -> Files {
        Files {
            vocabulary: vocabulary.to_owned(),
            learned: vocabulary.with_file_name("learned.md"),
        }
    }

    pub fn read(&self) -> io::Result<(Document, Document)> {
        Ok((
            Document::parse(&read(&self.vocabulary)?.unwrap_or_default()),
            Document::parse(&read(&self.learned)?.unwrap_or_default()),
        ))
    }

    /// Reads both files, applies `edit`, and writes back only the files it
    /// changed. A file that changes on disk between the read and the write is
    /// re-read and the edit applied again to the new bytes, so a sync landing
    /// mid-edit is merged rather than overwritten.
    pub fn edit(&self, edit: &Edit) -> Result<Vec<Note>, String> {
        let paths = [&self.vocabulary, &self.learned];
        let failed = |error: io::Error| format!("Unable to save: {error}.");
        for _ in 0..3 {
            let before = [
                read(paths[0]).map_err(failed)?,
                read(paths[1]).map_err(failed)?,
            ];
            let base = [
                before[0].as_deref().unwrap_or(NEW_VOCABULARY),
                before[1].as_deref().unwrap_or_default(),
            ];
            let (mut vocabulary, mut learned) =
                (Document::parse(base[0]), Document::parse(base[1]));
            let notes = edit.apply(&mut vocabulary, &mut learned)?;
            let after = [vocabulary.render(), learned.render()];
            let mut staged = Vec::new();
            for index in 0..2 {
                if after[index] == base[index] {
                    continue;
                }
                let temp = paths[index].with_extension("md.tmp");
                if let Some(parent) = paths[index].parent() {
                    fs::create_dir_all(parent).map_err(failed)?;
                }
                fs::write(&temp, &after[index]).map_err(failed)?;
                staged.push((temp, index));
            }
            let raced = staged
                .iter()
                .any(|&(_, index)| read(paths[index]).ok().as_ref() != Some(&before[index]));
            if raced {
                for (temp, _) in &staged {
                    let _ = fs::remove_file(temp);
                }
                continue;
            }
            for (temp, index) in staged {
                fs::rename(temp, paths[index]).map_err(failed)?;
            }
            return Ok(notes);
        }
        Err("The files kept changing while saving. Try again.".to_owned())
    }
}

/// What a vocabulary starts as when the window adds the first word.
pub const NEW_VOCABULARY: &str = "# see.computer vocabulary\n# One term per line\n";

fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_word_that_gains_a_replacement_is_not_forgotten() {
        let mut vocabulary = Document::parse("- Zentrix\n");
        let mut learned = Document::default();
        let notes = Edit::Change {
            term: "Zentrix".into(),
            heard: None,
            to: "zentrixx -> Zentrix".into(),
        }
        .apply(&mut vocabulary, &mut learned)
        .unwrap();
        assert_eq!(
            notes,
            [Note::Added("Zentrix".into(), Some("zentrixx".into()))]
        );
        assert_eq!(vocabulary.render(), "- zentrixx -> Zentrix\n");
    }

    #[test]
    fn pair_edits_preserve_position_weights_and_comments() {
        let mut vocabulary =
            Document::parse("# - Zentrix\t4\r\n- Quorvo\n- zentrixx -> Zentrix\t2.00\n- Veltrix\n");
        let mut learned = Document::parse("- Zentrix\n");
        Edit::Change {
            term: "ZENTRIX".into(),
            heard: Some("ZENTRIXX".into()),
            to: "zen tricks -> Zentrix".into(),
        }
        .apply(&mut vocabulary, &mut learned)
        .unwrap();
        assert_eq!(
            vocabulary.render(),
            "# - Zentrix\t4\r\n- Quorvo\n- zen tricks -> Zentrix\t2.00\n- Veltrix\n"
        );
        assert!(Edit::Change {
            term: "Zentrix".into(),
            heard: Some("zentrixx".into()),
            to: "Quorvo".into()
        }
        .apply(&mut vocabulary, &mut learned)
        .is_err());
        assert!(Edit::Change {
            term: "Zentrix".into(),
            heard: Some("zen tricks".into()),
            to: "Quorvo".into()
        }
        .apply(&mut vocabulary, &mut learned)
        .is_err());
        Edit::Delete {
            term: "Zentrix".into(),
            heard: Some("zen tricks".into()),
        }
        .apply(&mut vocabulary, &mut learned)
        .unwrap();
        Edit::Add {
            text: "Zentrix".into(),
        }
        .apply(&mut Document::default(), &mut Document::default())
        .unwrap();
        assert_eq!(
            vocabulary.render(),
            "# - Zentrix\t4\r\n- Quorvo\n- Veltrix\n"
        );
        assert_eq!(learned.render(), "- Zentrix\n");
        assert_eq!(Line::parse("# - Zentrix"), Line::Other);
    }
    #[test]
    fn delete_removes_pair_from_both_files_only() {
        let mut a = Document::parse("- Zentrix\n- zentrixx -> Zentrix\n");
        let mut b = a.clone();
        let notes = Edit::Delete {
            term: "Zentrix".into(),
            heard: Some("zentrixx".into()),
        }
        .apply(&mut a, &mut b)
        .unwrap();
        assert_eq!(a.render(), "- Zentrix\n");
        assert_eq!(b.render(), "- Zentrix\n");
        assert_eq!(
            notes,
            [Note::Forgot("Zentrix".into(), Some("zentrixx".into()))]
        );
    }
}
