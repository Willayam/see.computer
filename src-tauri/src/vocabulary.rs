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
    /// `# - Term`: an entry the user switched off by commenting it out.
    Parked(Entry),
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
        if let Some(comment) = line.strip_prefix('#') {
            return comment
                .trim_start()
                .strip_prefix("- ")
                .and_then(entry)
                .map_or(Line::Other, Line::Parked);
        }
        entry(line.strip_prefix("- ").unwrap_or(line)).map_or(Line::Other, Line::Active)
    }

    fn entry(&self) -> Option<&Entry> {
        match self {
            Line::Active(entry) | Line::Parked(entry) => Some(entry),
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

    fn parked(&self, term: &str) -> Vec<usize> {
        self.find(|line| matches!(line, Line::Parked(e) if same(&e.term, term)))
    }

    pub fn has_active(&self, term: &str) -> bool {
        !self.active(term).is_empty()
    }

    pub fn has_parked(&self, term: &str) -> bool {
        !self.parked(term).is_empty()
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
        let bulleted = !matches!(Line::parse(body(chunk)), Line::Active(_))
            || chunk.trim_start().starts_with("- ");
        let text = render_line(line, bulleted);
        self.chunks[index] = format!("{indent}{text}{}", ending(chunk));
    }

    /// Puts a new line after the last line of the same kind, or at the end,
    /// so a new term joins the list and a parked one joins the parked block.
    fn insert(&mut self, line: &Line) {
        let parked = matches!(line, Line::Parked(_));
        let after = self
            .lines()
            .enumerate()
            .filter(|(_, l)| match l {
                Line::Active(_) => !parked,
                Line::Parked(_) => parked,
                Line::Other => false,
            })
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

    /// Adds `entry` as an active line. Returns false when the term is already
    /// active here.
    pub fn add(&mut self, entry: Entry) -> bool {
        if self.has_active(&entry.term) {
            return false;
        }
        self.insert(&Line::Active(entry));
        true
    }

    /// Every line for `term`, active or parked, now spells it `to`.
    pub fn rename(&mut self, term: &str, to: &str) -> bool {
        let indices = self.find(|line| line.entry().is_some_and(|e| same(&e.term, term)));
        for &index in &indices {
            let mut line = Line::parse(body(&self.chunks[index]));
            if let Line::Active(e) | Line::Parked(e) = &mut line {
                e.term = to.to_owned();
            }
            self.rewrite(index, &line);
        }
        !indices.is_empty()
    }

    pub fn set_weight(&mut self, term: &str, weight: f32) -> bool {
        let indices = self.active(term);
        for &index in &indices {
            let mut entry = self.entry_at(index);
            entry.weight = weight;
            self.rewrite(index, &Line::Active(entry));
        }
        !indices.is_empty()
    }

    /// Drops every active line for `term`. Parked lines stay.
    pub fn delete(&mut self, term: &str) -> bool {
        let indices = self.active(term);
        self.remove(&indices);
        !indices.is_empty()
    }

    pub fn delete_parked(&mut self, term: &str) -> bool {
        let indices = self.parked(term);
        self.remove(&indices);
        !indices.is_empty()
    }

    /// Comments the term's lines out and moves them to the parked block.
    pub fn park(&mut self, term: &str) -> bool {
        let indices = self.active(term);
        if indices.is_empty() {
            return false;
        }
        self.delete_parked(term);
        let indices = self.active(term);
        let entries: Vec<_> = indices.iter().map(|&i| self.entry_at(i)).collect();
        self.remove(&indices);
        for entry in entries {
            self.insert(&Line::Parked(entry));
        }
        !indices.is_empty()
    }

    /// The reverse of `park`. A parked copy of a term that is already active
    /// is just dropped.
    pub fn unpark(&mut self, term: &str) -> bool {
        let indices = self.parked(term);
        let entries: Vec<_> = indices.iter().map(|&i| self.entry_at(i)).collect();
        self.remove(&indices);
        let active = self.has_active(term);
        for entry in entries.into_iter().filter(|_| !active) {
            self.insert(&Line::Active(entry));
        }
        !indices.is_empty()
    }

    /// Changes, removes, or adds the `heard -> term` rule. Removing the only
    /// line that names the term keeps the term and drops the rule; adding to a
    /// term with a plain line turns that line into the rule. Returns false
    /// when the file holds nothing the change could apply to.
    pub fn set_rule(&mut self, term: &str, from: Option<&str>, to: Option<&str>) -> bool {
        let active = self.active(term);
        let with_rule = |doc: &Document, heard: &str| {
            active
                .iter()
                .copied()
                .filter(|&i| {
                    doc.entry_at(i)
                        .heard
                        .is_some_and(|h| h.eq_ignore_ascii_case(heard))
                })
                .collect::<Vec<_>>()
        };
        match (from, to) {
            (Some(from), Some(to)) => {
                let indices = with_rule(self, from);
                for &index in &indices {
                    let mut entry = self.entry_at(index);
                    entry.heard = Some(to.to_owned());
                    self.rewrite(index, &Line::Active(entry));
                }
                !indices.is_empty()
            }
            (Some(from), None) => {
                let indices = with_rule(self, from);
                let Some((&keep, rest)) = indices.split_first() else {
                    return false;
                };
                if active.len() > indices.len() {
                    // The boost a rule line carried stays with the term.
                    let weight = indices
                        .iter()
                        .map(|&i| self.entry_at(i).weight)
                        .fold(0.0, f32::max);
                    self.remove(&indices);
                    let kept = self
                        .active(term)
                        .iter()
                        .map(|&i| self.entry_at(i).weight)
                        .fold(0.0, f32::max);
                    if weight > kept {
                        self.set_weight(term, weight);
                    }
                } else {
                    let mut entry = self.entry_at(keep);
                    entry.heard = None;
                    self.rewrite(keep, &Line::Active(entry));
                    self.remove(rest);
                }
                true
            }
            (None, Some(to)) => {
                if !with_rule(self, to).is_empty() {
                    return true;
                }
                let plain = active
                    .iter()
                    .copied()
                    .find(|&i| self.entry_at(i).heard.is_none());
                match (plain, active.last()) {
                    (Some(index), _) => {
                        let mut entry = self.entry_at(index);
                        entry.heard = Some(to.to_owned());
                        self.rewrite(index, &Line::Active(entry));
                    }
                    (None, Some(&last)) => {
                        let mut entry = self.entry_at(last);
                        entry.heard = Some(to.to_owned());
                        let newline = self.newline();
                        if ending(&self.chunks[last]).is_empty() {
                            self.chunks[last].push_str(newline);
                        }
                        self.chunks.insert(
                            last + 1,
                            format!("{}{newline}", render_line(&Line::Active(entry), true)),
                        );
                    }
                    (None, None) => return false,
                }
                true
            }
            (None, None) => false,
        }
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
        Line::Parked(entry) => format!("# - {}", render(entry)),
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

/// What the window asks for, named by the term as it was shown.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Edit {
    /// A term, or a `heard -> Term` line, typed into the add field.
    Add {
        text: String,
    },
    Rename {
        term: String,
        to: String,
    },
    Weight {
        term: String,
        weight: f32,
    },
    Delete {
        term: String,
    },
    Park {
        term: String,
    },
    Unpark {
        term: String,
    },
    /// `from` is the heard form being changed or removed, `to` its new form;
    /// no `from` adds a rule.
    Rule {
        term: String,
        from: Option<String>,
        to: Option<String>,
    },
    /// A word seen once, accepted now.
    Learn {
        term: String,
    },
    /// A word seen once, never to be offered again.
    Ignore {
        term: String,
    },
}

/// What an edit means for the event log, beyond the files.
#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    /// A learned spelling the learner must not bring back.
    Forgot(String),
    /// A learned term now spelled this way.
    Learned(String),
}

fn stale(term: &str) -> String {
    format!("\u{201c}{term}\u{201d} changed on disk, so the list has been reloaded.")
}

impl Edit {
    /// Trims what the window sent and refuses text the files cannot hold.
    pub fn checked(self) -> Result<Edit, String> {
        fn clean(text: String, what: &str) -> Result<String, String> {
            let text = text.trim().to_owned();
            if text.is_empty() {
                return Err(format!("Type a {what}."));
            }
            if text.contains(['\t', '\n', '\r']) || text.starts_with('#') {
                return Err(format!(
                    "Use a {what} without tabs, line breaks, or a leading #."
                ));
            }
            Ok(text)
        }
        fn word(text: String) -> Result<String, String> {
            let text = clean(text, "word")?;
            if text.contains(" -> ") {
                return Err("Add a heard form under Heard as instead.".to_owned());
            }
            if text.starts_with("- ") {
                return Err("Use a word without a leading dash.".to_owned());
            }
            Ok(text)
        }
        let heard = |text: Option<String>| {
            text.map(|t| clean(t, "heard form"))
                .transpose()
                .and_then(|t| match t {
                    Some(t) if t.contains(" -> ") => {
                        Err("Use a heard form without \" -> \".".to_owned())
                    }
                    t => Ok(t),
                })
        };
        Ok(match self {
            Edit::Add { text } => {
                let text = clean(text, "word")?;
                let (said, term) = match text.split_once(" -> ") {
                    Some((said, term)) => (heard(Some(said.to_owned()))?, term.to_owned()),
                    None => (None, text.clone()),
                };
                let term = word(term)?;
                Edit::Add {
                    text: match said {
                        Some(said) => format!("{said} -> {term}"),
                        None => term,
                    },
                }
            }
            Edit::Rename { term, to } => Edit::Rename {
                term: word(term)?,
                to: word(to)?,
            },
            Edit::Weight { term, weight } => {
                if !(weight.is_finite() && (0.1..=100.0).contains(&weight)) {
                    return Err("Choose a boost between 0.1 and 100.".to_owned());
                }
                Edit::Weight {
                    term: word(term)?,
                    weight: (weight * 100.0).round() / 100.0,
                }
            }
            Edit::Delete { term } => Edit::Delete { term: word(term)? },
            Edit::Park { term } => Edit::Park { term: word(term)? },
            Edit::Unpark { term } => Edit::Unpark { term: word(term)? },
            Edit::Rule { term, from, to } => {
                let (from, to) = (heard(from)?, heard(to)?);
                if from.is_none() && to.is_none() {
                    return Err("Type a heard form.".to_owned());
                }
                Edit::Rule {
                    term: word(term)?,
                    from,
                    to,
                }
            }
            Edit::Learn { term } => Edit::Learn { term: word(term)? },
            Edit::Ignore { term } => Edit::Ignore { term: word(term)? },
        })
    }

    /// Applies a file edit to both files. `Learn` and `Ignore` touch only the
    /// event log and are the ledger's to handle.
    pub fn apply(
        &self,
        vocabulary: &mut Document,
        learned: &mut Document,
    ) -> Result<Vec<Note>, String> {
        let known = |doc: &Document, term: &str| doc.has_active(term) || doc.has_parked(term);
        match self {
            Edit::Add { text } => {
                let Line::Active(entry) = Line::parse(&format!("- {text}")) else {
                    return Err("Type a word.".to_owned());
                };
                if let Some(heard) = &entry.heard {
                    if learned.has_active(&entry.term) || vocabulary.has_active(&entry.term) {
                        return Edit::Rule {
                            term: entry.term.clone(),
                            from: None,
                            to: Some(heard.clone()),
                        }
                        .apply(vocabulary, learned);
                    }
                }
                if vocabulary.has_active(&entry.term) || learned.has_active(&entry.term) {
                    return Err(format!(
                        "\u{201c}{}\u{201d} is already in your dictionary.",
                        entry.term
                    ));
                }
                if vocabulary.has_parked(&entry.term) {
                    vocabulary.unpark(&entry.term);
                    vocabulary.rename(&entry.term, &entry.term);
                    if let Some(heard) = &entry.heard {
                        vocabulary.set_rule(&entry.term, None, Some(heard));
                    }
                } else {
                    vocabulary.add(entry);
                }
                Ok(Vec::new())
            }
            Edit::Rename { term, to } => {
                if !same(term, to) && (known(vocabulary, to) || known(learned, to)) {
                    return Err(format!(
                        "\u{201c}{to}\u{201d} is already in your dictionary."
                    ));
                }
                let in_vocabulary = vocabulary.rename(term, to);
                let learned_spelling = learned.lines().find_map(|line| match line {
                    Line::Active(e) if same(&e.term, term) => Some(e.term),
                    _ => None,
                });
                let in_learned = learned.rename(term, to);
                if !in_vocabulary && !in_learned {
                    return Err(stale(term));
                }
                Ok(match learned_spelling {
                    Some(old) if old != *to => vec![Note::Forgot(old), Note::Learned(to.clone())],
                    _ => Vec::new(),
                })
            }
            Edit::Weight { term, weight } => {
                let a = vocabulary.set_weight(term, *weight);
                let b = learned.set_weight(term, *weight);
                if a || b {
                    Ok(Vec::new())
                } else {
                    Err(stale(term))
                }
            }
            Edit::Delete { term } => {
                let spellings: Vec<_> = learned
                    .lines()
                    .filter_map(|line| match line {
                        Line::Active(e) if same(&e.term, term) => Some(e.term),
                        _ => None,
                    })
                    .collect();
                let a = vocabulary.delete(term);
                let b = learned.delete(term);
                let c = vocabulary.delete_parked(term);
                if !(a || b || c) {
                    return Err(stale(term));
                }
                let mut notes: Vec<_> = Vec::new();
                for spelling in spellings {
                    let note = Note::Forgot(spelling);
                    if !notes.contains(&note) {
                        notes.push(note);
                    }
                }
                Ok(notes)
            }
            Edit::Park { term } => {
                if learned.has_active(term) {
                    return Err("Learned words can't be parked. Delete it instead.".to_owned());
                }
                if vocabulary.park(term) {
                    Ok(Vec::new())
                } else {
                    Err(stale(term))
                }
            }
            Edit::Unpark { term } => {
                if vocabulary.unpark(term) {
                    Ok(Vec::new())
                } else {
                    Err(stale(term))
                }
            }
            Edit::Rule { term, from, to } => {
                let changed = match from {
                    Some(_) => {
                        let a = vocabulary.set_rule(term, from.as_deref(), to.as_deref());
                        let b = learned.set_rule(term, from.as_deref(), to.as_deref());
                        a || b
                    }
                    None if vocabulary.has_active(term) => {
                        vocabulary.set_rule(term, None, to.as_deref())
                    }
                    None => learned.set_rule(term, None, to.as_deref()),
                };
                if changed {
                    Ok(Vec::new())
                } else {
                    Err(stale(term))
                }
            }
            Edit::Learn { .. } | Edit::Ignore { .. } => Ok(Vec::new()),
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

    const VOCABULARY: &str = "# see.computer vocabulary\n\
        #\n\
        # One per line.\n\
        \n\
        - Tauri\n\
        - pnpm\t4\n\
        - potato -> poteto\n\
        \n\
        # Parked: short words. Uncomment any you want back.\n\
        \n\
        # - EAS\n\
        # - by the way\n";

    fn doc(text: &str) -> Document {
        Document::parse(text)
    }

    fn active(term: &str) -> Entry {
        Entry {
            heard: None,
            term: term.into(),
            weight: 1.0,
        }
    }

    fn apply(
        edit: Edit,
        vocabulary: &str,
        learned: &str,
    ) -> (String, String, Result<Vec<Note>, String>) {
        let (mut v, mut l) = (doc(vocabulary), doc(learned));
        let result = edit.checked().and_then(|edit| edit.apply(&mut v, &mut l));
        (v.render(), l.render(), result)
    }

    #[test]
    fn lines_read_the_way_the_decoder_reads_them() {
        let lines: Vec<_> = doc(VOCABULARY).lines().collect();
        assert_eq!(lines[0], Line::Other);
        assert_eq!(lines[4], Line::Active(active("Tauri")));
        assert_eq!(
            lines[5],
            Line::Active(Entry {
                weight: 4.0,
                ..active("pnpm")
            })
        );
        assert_eq!(
            lines[6],
            Line::Active(Entry {
                heard: Some("potato".into()),
                ..active("poteto")
            })
        );
        assert_eq!(lines[8], Line::Other, "a prose comment is not parked");
        assert_eq!(lines[10], Line::Parked(active("EAS")));
        assert_eq!(lines[11], Line::Parked(active("by the way")));
        assert_eq!(Line::parse("- Off\t0"), Line::Other);
        assert_eq!(Line::parse("Bare"), Line::Active(active("Bare")));
    }

    #[test]
    fn an_untouched_document_renders_its_own_bytes() {
        for text in [VOCABULARY, "", "- a\r\n- b", "no newline", "\n\n"] {
            assert_eq!(doc(text).render(), text);
        }
    }

    #[test]
    fn adding_lands_after_the_last_term_and_leaves_the_rest_alone() {
        let (v, _, result) = apply(
            Edit::Add {
                text: " Convex ".into(),
            },
            VOCABULARY,
            "",
        );
        assert_eq!(result, Ok(vec![]));
        assert_eq!(
            v,
            VOCABULARY.replace("- potato -> poteto\n", "- potato -> poteto\n- Convex\n")
        );
        let (v, _, _) = apply(
            Edit::Add {
                text: "Convex".into(),
            },
            "- a\r\n- b",
            "",
        );
        assert_eq!(v, "- a\r\n- b\r\n- Convex\r\n");
        let (_, _, result) = apply(
            Edit::Add {
                text: "tauri".into(),
            },
            VOCABULARY,
            "",
        );
        assert_eq!(
            result,
            Err("\u{201c}tauri\u{201d} is already in your dictionary.".into())
        );
    }

    #[test]
    fn adding_a_parked_word_turns_it_back_on() {
        let (v, _, result) = apply(Edit::Add { text: "EAS".into() }, VOCABULARY, "");
        assert_eq!(result, Ok(vec![]));
        assert_eq!(
            v,
            VOCABULARY
                .replace("# - EAS\n", "")
                .replace("- potato -> poteto\n", "- potato -> poteto\n- EAS\n")
        );
    }

    #[test]
    fn a_casing_change_replaces_the_spelling_in_both_files() {
        let learned = "# learned\n- superbase -> Supabase\n- Poteto\n";
        let (v, l, result) = apply(
            Edit::Rename {
                term: "Poteto".into(),
                to: "POTETO".into(),
            },
            VOCABULARY,
            learned,
        );
        assert_eq!(v, VOCABULARY.replace("-> poteto", "-> POTETO"));
        assert_eq!(l, learned.replace("- Poteto", "- POTETO"));
        assert_eq!(
            result,
            Ok(vec![
                Note::Forgot("Poteto".into()),
                Note::Learned("POTETO".into())
            ])
        );
        let (_, _, result) = apply(
            Edit::Rename {
                term: "Tauri".into(),
                to: "pnpm".into(),
            },
            VOCABULARY,
            "",
        );
        assert!(result.is_err(), "renaming onto another word is refused");
    }

    #[test]
    fn an_edit_to_a_word_that_is_gone_is_stale() {
        for edit in [
            Edit::Rename {
                term: "Vanished".into(),
                to: "Other".into(),
            },
            Edit::Delete {
                term: "Vanished".into(),
            },
            Edit::Weight {
                term: "Vanished".into(),
                weight: 2.0,
            },
        ] {
            let (v, l, result) = apply(edit, VOCABULARY, "");
            assert_eq!(result, Err(stale("Vanished")));
            assert_eq!((v.as_str(), l.as_str()), (VOCABULARY, ""));
        }
    }

    #[test]
    fn weight_one_drops_the_number() {
        let (v, _, _) = apply(
            Edit::Weight {
                term: "PNPM".into(),
                weight: 1.0,
            },
            VOCABULARY,
            "",
        );
        assert_eq!(v, VOCABULARY.replace("- pnpm\t4\n", "- pnpm\n"));
        let (v, _, _) = apply(
            Edit::Weight {
                term: "Tauri".into(),
                weight: 2.5,
            },
            VOCABULARY,
            "",
        );
        assert_eq!(v, VOCABULARY.replace("- Tauri\n", "- Tauri\t2.5\n"));
    }

    #[test]
    fn park_and_unpark_move_the_line_between_blocks() {
        let (parked, _, result) = apply(
            Edit::Park {
                term: "pnpm".into(),
            },
            VOCABULARY,
            "",
        );
        assert_eq!(result, Ok(vec![]));
        assert_eq!(
            parked,
            VOCABULARY
                .replace("- pnpm\t4\n", "")
                .replace("# - by the way\n", "# - by the way\n# - pnpm\t4\n")
        );
        let (back, _, _) = apply(
            Edit::Unpark {
                term: "pnpm".into(),
            },
            &parked,
            "",
        );
        assert_eq!(
            back,
            VOCABULARY
                .replace("- pnpm\t4\n", "")
                .replace("- potato -> poteto\n", "- potato -> poteto\n- pnpm\t4\n")
        );
        let (_, _, result) = apply(
            Edit::Park {
                term: "Supabase".into(),
            },
            VOCABULARY,
            "- Supabase\n",
        );
        assert!(result.is_err(), "a learned word is deleted, not parked");
    }

    #[test]
    fn rules_are_edited_removed_and_added_in_place() {
        let rule = |from: Option<&str>, to: Option<&str>| Edit::Rule {
            term: "poteto".into(),
            from: from.map(Into::into),
            to: to.map(Into::into),
        };
        let (v, _, _) = apply(rule(Some("potato"), Some("pateto")), VOCABULARY, "");
        assert_eq!(
            v,
            VOCABULARY.replace("- potato -> poteto", "- pateto -> poteto")
        );
        let (v, _, _) = apply(rule(Some("Potato"), None), VOCABULARY, "");
        assert_eq!(
            v,
            VOCABULARY.replace("- potato -> poteto", "- poteto"),
            "the last rule for a term leaves the term behind"
        );
        let (v, _, _) = apply(rule(None, Some("potato mode")), VOCABULARY, "");
        assert_eq!(
            v,
            VOCABULARY.replace(
                "- potato -> poteto\n",
                "- potato -> poteto\n- potato mode -> poteto\n"
            )
        );
        let two = "- potato -> poteto\n- pateto -> poteto\n";
        let (_, l, _) = apply(rule(Some("pateto"), None), "", two);
        assert_eq!(l, "- potato -> poteto\n");
        let (v, _, _) = apply(
            Edit::Rule {
                term: "Tauri".into(),
                from: None,
                to: Some("tory".into()),
            },
            VOCABULARY,
            "",
        );
        assert_eq!(v, VOCABULARY.replace("- Tauri\n", "- tory -> Tauri\n"));
    }

    #[test]
    fn deleting_a_learned_word_forgets_it() {
        let (v, l, result) = apply(
            Edit::Delete {
                term: "supabase".into(),
            },
            "- Supabase\n- Tauri\n",
            "# learned\n- superbase -> Supabase\n",
        );
        assert_eq!((v.as_str(), l.as_str()), ("- Tauri\n", "# learned\n"));
        assert_eq!(result, Ok(vec![Note::Forgot("Supabase".into())]));
        let (v, _, result) = apply(Edit::Delete { term: "EAS".into() }, VOCABULARY, "");
        assert_eq!(v, VOCABULARY.replace("# - EAS\n", ""));
        assert_eq!(result, Ok(vec![]));
    }

    #[test]
    fn adding_a_rule_for_a_parked_word_keeps_the_rule() {
        let (v, _, _) = apply(
            Edit::Add {
                text: "ease -> EAS".into(),
            },
            VOCABULARY,
            "",
        );
        assert_eq!(
            v,
            VOCABULARY.replace("# - EAS\n", "").replace(
                "- potato -> poteto\n",
                "- potato -> poteto\n- ease -> EAS\n"
            )
        );
    }

    #[test]
    fn removing_a_rule_keeps_the_boost_it_carried() {
        let (v, _, _) = apply(
            Edit::Rule {
                term: "Tauri".into(),
                from: Some("tory".into()),
                to: None,
            },
            "- tory -> Tauri\t4\n- Tauri\n",
            "",
        );
        assert_eq!(v, "- Tauri\t4\n");
    }

    #[test]
    fn a_word_both_active_and_parked_is_deleted_and_parked_once() {
        let both = "- EAS\n# - EAS\n";
        let (v, _, _) = apply(Edit::Delete { term: "EAS".into() }, both, "");
        assert_eq!(v, "");
        let (v, _, _) = apply(Edit::Park { term: "EAS".into() }, both, "");
        assert_eq!(v, "# - EAS\n");
    }

    #[test]
    fn text_the_files_cannot_hold_is_refused() {
        for text in ["a -> b -> c", "x -> #y", "- x"] {
            assert!(
                Edit::Add { text: text.into() }.checked().is_err(),
                "{text:?}"
            );
        }
        assert!(Edit::Rename {
            term: "Tauri".into(),
            to: "- x".into()
        }
        .checked()
        .is_err());
        for text in ["", "  ", "# heading", "a\tb", "two\nlines"] {
            assert!(
                Edit::Add { text: text.into() }.checked().is_err(),
                "{text:?}"
            );
        }
        assert!(Edit::Weight {
            term: "Tauri".into(),
            weight: 0.0
        }
        .checked()
        .is_err());
    }

    #[test]
    fn an_edit_applies_to_the_bytes_on_disk_now() {
        let root = std::env::temp_dir().join(format!(
            "see-vocabulary-{}-{}",
            std::process::id(),
            chrono::Local::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        let files = Files::beside(&root.join("vocabulary.md"));
        fs::write(
            &files.vocabulary,
            VOCABULARY.replace("- Tauri\n", "- Tauri\n- Synced\n"),
        )
        .unwrap();
        assert_eq!(
            files.edit(&Edit::Delete {
                term: "pnpm".into()
            }),
            Ok(vec![])
        );
        assert_eq!(
            fs::read_to_string(&files.vocabulary).unwrap(),
            VOCABULARY
                .replace("- Tauri\n", "- Tauri\n- Synced\n")
                .replace("- pnpm\t4\n", "")
        );
        assert!(!files.learned.exists(), "an untouched file is not created");
        fs::remove_dir_all(&root).unwrap();
        let fresh = std::env::temp_dir().join(format!("see-vocabulary-new-{}", std::process::id()));
        let files = Files::beside(&fresh.join("vocabulary.md"));
        assert_eq!(
            files.edit(&Edit::Add {
                text: "Tauri".into()
            }),
            Ok(vec![])
        );
        assert_eq!(
            fs::read_to_string(&files.vocabulary).unwrap(),
            format!("{NEW_VOCABULARY}- Tauri\n")
        );
        fs::remove_dir_all(&fresh).unwrap();
    }
}
