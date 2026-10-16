//! Every dictation as the learning loop needs it: what was heard, where it
//! went, what the user turned it into, and the audio to replay it.
//!
//! Append-only events, so the learner and the eval corpus are derived and can
//! be rebuilt from the log. Kept in Application Support rather than beside the
//! history in Documents, because Documents may sync to iCloud and audio is
//! large and private.

use crate::pill::{Notice, PillEvent};
use chrono::{Local, TimeZone};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct Id(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    ReadBack,
    Manual,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    Dictated {
        id: Id,
        at: String,
        text: String,
        audio: Option<String>,
    },
    Landed {
        id: Id,
        app: String,
        bundle: String,
        focus: String,
        landing: String,
    },
    Context {
        id: Id,
        terms: Vec<ContextHit>,
    },
    Corrected {
        id: Id,
        at: String,
        text: String,
        source: Source,
    },
    Learned {
        at: String,
        term: String,
        heard: Option<String>,
        from: Vec<Id>,
    },
    Forgotten {
        at: String,
        term: String,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContextHit {
    pub term: String,
    pub source: String,
    pub landed: bool,
}

enum Command {
    Dictated(Event, crate::mic::Audio16k),
    Event(Event),
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
    Forget(String),
    Edit(crate::vocabulary::Edit, mpsc::Sender<Result<(), String>>),
    Fixed(Fixed),
}

/// A transcript line the user fixed in the Library, which may be a dictation
/// from before the ledger existed or a meeting line it never saw.
struct Fixed {
    at_ms: i64,
    original: String,
    edited: String,
}

#[derive(Clone)]
pub struct Ledger {
    tx: mpsc::Sender<Command>,
    enabled: Arc<AtomicBool>,
    last_millis: Arc<AtomicI64>,
    learning: Arc<Mutex<Learning>>,
}

#[derive(Clone, Default)]
pub struct Learning {
    pub learned: Vec<String>,
    pub waiting: Vec<crate::learn::Waiting>,
    /// What the model wrote in place of each term the user corrected.
    pub heard: Vec<(String, Vec<String>)>,
}

impl Ledger {
    pub fn start(enabled: Arc<AtomicBool>, pill: Option<mpsc::Sender<PillEvent>>) -> Ledger {
        let root = std::env::var_os("SEE_COMPUTER_LEDGER_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(crate::paths::ledger);
        Self::start_at(root, enabled, 500, None, pill)
    }

    pub(crate) fn start_at(
        root: PathBuf,
        enabled: Arc<AtomicBool>,
        keep_audio: usize,
        vocabulary: Option<PathBuf>,
        pill: Option<mpsc::Sender<PillEvent>>,
    ) -> Ledger {
        let vocabulary = vocabulary.unwrap_or_else(crate::boost::Lexicon::default_path);
        let learning = Arc::new(Mutex::new(Learning::default()));
        let snapshot = learning.clone();
        let (tx, rx) = mpsc::channel();
        crate::qos::spawn("see-ledger", crate::qos::Class::Upkeep, move || {
            let _ = fs::create_dir_all(root.join("audio"));
            let mut events = read(&root);
            let words = crate::learn::Words::system();
            let mut corrected = BTreeMap::new();
            for event in &events {
                if let Event::Corrected { id, text, .. } = event {
                    corrected.insert(id.clone(), text.clone());
                }
            }
            update_learning(&snapshot, &events, &vocabulary, &words);
            let _ = write_corpus(&root, &corrected);
            prune(&root, keep_audio, 50, &corrected.keys().cloned().collect());
            let _ = compact(&root, &mut events, 500, 50);
            while let Ok(command) = rx.recv() {
                match command {
                    Command::Dictated(mut event, audio) => {
                        if let Event::Dictated {
                            id, audio: path, ..
                        } = &mut event
                        {
                            let relative = format!("audio/{}.wav", id.0);
                            if write_audio(&root.join(&relative), &audio).is_ok() {
                                *path = Some(relative);
                            }
                        }
                        if append(&root, &event).is_ok() {
                            events.push(event);
                        }
                        let protected = corrected.keys().cloned().collect();
                        prune(&root, keep_audio, 50, &protected);
                        let _ = compact(&root, &mut events, 500, 50);
                    }
                    Command::Event(event) => {
                        if append(&root, &event).is_ok() {
                            let read_back = match &event {
                                Event::Corrected {
                                    id, text, source, ..
                                } => {
                                    corrected.insert(id.clone(), text.clone());
                                    let _ = write_corpus(&root, &corrected);
                                    Some((id.clone(), text.clone(), *source == Source::ReadBack))
                                }
                                _ => None,
                            };
                            events.push(event);
                            if let Some((id, text, read_back)) = read_back {
                                let names = promote(&root, &vocabulary, &words, &mut events);
                                // A fix made in another app gets the card, which says
                                // what was seen and opens it; one made here gets a flash.
                                let fix = read_back
                                    .then(|| {
                                        fix_notice(&events, &id, &text, &vocabulary, &words, &names)
                                    })
                                    .flatten();
                                match (fix, &pill) {
                                    (Some(fix), Some(pill)) => {
                                        // Words learned that the card does not name
                                        // still get their flash, before it.
                                        let others: Vec<_> = names
                                            .iter()
                                            .filter(|n| **n != fix.wrote)
                                            .cloned()
                                            .collect();
                                        announce(&Some(pill.clone()), &others);
                                        let _ = pill.send(PillEvent::Fixed(fix));
                                    }
                                    _ => announce(&pill, &names),
                                }
                            }
                            update_learning(&snapshot, &events, &vocabulary, &words);
                        }
                    }
                    Command::Fixed(fixed) => {
                        let id = match dictation_at(&events, &fixed) {
                            Some(id) => Some(id),
                            None => {
                                let id = unused_id(&events, fixed.at_ms);
                                let event = Event::Dictated {
                                    id: id.clone(),
                                    at: Local
                                        .timestamp_millis_opt(fixed.at_ms)
                                        .single()
                                        .unwrap_or_else(Local::now)
                                        .to_rfc3339(),
                                    text: fixed.original.trim().to_owned(),
                                    audio: None,
                                };
                                append(&root, &event).is_ok().then(|| {
                                    events.push(event);
                                    id
                                })
                            }
                        };
                        let event = id.map(|id| Event::Corrected {
                            id,
                            at: Local::now().to_rfc3339(),
                            text: fixed.edited.trim().to_owned(),
                            source: Source::Manual,
                        });
                        if let Some(event) = event.filter(|event| append(&root, event).is_ok()) {
                            if let Event::Corrected { id, text, .. } = &event {
                                corrected.insert(id.clone(), text.clone());
                                let _ = write_corpus(&root, &corrected);
                            }
                            events.push(event);
                            let names = promote(&root, &vocabulary, &words, &mut events);
                            announce(&pill, &names);
                            update_learning(&snapshot, &events, &vocabulary, &words);
                        }
                    }
                    Command::Forget(term) => {
                        match remove_term(&vocabulary.with_file_name("learned.md"), &term) {
                            Ok(spellings) => {
                                for spelling in spellings {
                                    let event = Event::Forgotten {
                                        at: Local::now().to_rfc3339(),
                                        term: spelling,
                                    };
                                    if append(&root, &event).is_ok() {
                                        events.push(event);
                                    }
                                }
                                update_learning(&snapshot, &events, &vocabulary, &words);
                            }
                            Err(error) => eprintln!("could not forget {term}: {error}"),
                        }
                    }
                    Command::Edit(edit, reply) => {
                        let result =
                            apply_edit(&root, &vocabulary, &words, &mut events, &pill, edit);
                        update_learning(&snapshot, &events, &vocabulary, &words);
                        let _ = reply.send(result);
                    }
                    #[cfg(test)]
                    Command::Flush(reply) => {
                        let _ = reply.send(());
                    }
                }
            }
        });
        Ledger {
            tx,
            enabled,
            last_millis: Arc::new(AtomicI64::new(0)),
            learning,
        }
    }

    #[cfg(test)]
    pub fn off() -> Ledger {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        Ledger {
            tx,
            enabled: Arc::new(AtomicBool::new(false)),
            last_millis: Arc::new(AtomicI64::new(0)),
            learning: Arc::new(Mutex::new(Learning::default())),
        }
    }

    pub fn learning(&self) -> Learning {
        self.learning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn forget(&self, term: &str) {
        let _ = self.tx.send(Command::Forget(term.to_owned()));
    }
    /// Hands an edit of the vocabulary files to the ledger thread, which is
    /// the only writer of `learned.md` in this process. The receiver gets the
    /// outcome once the files and the log agree; dropping it is fine.
    pub fn edit(&self, edit: crate::vocabulary::Edit) -> mpsc::Receiver<Result<(), String>> {
        let (reply, outcome) = mpsc::channel();
        if let Err(mpsc::SendError(Command::Edit(_, reply))) =
            self.tx.send(Command::Edit(edit, reply))
        {
            let _ = reply.send(Err(
                "Unable to save. Quit and reopen see.computer, then try again.".to_owned(),
            ));
        }
        outcome
    }

    pub fn mint(&self) -> Id {
        let value = loop {
            let previous = self.last_millis.load(Ordering::Relaxed);
            let next = Local::now().timestamp_millis().max(previous + 1);
            if self
                .last_millis
                .compare_exchange(previous, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break next;
            }
        };
        let at = Local
            .timestamp_millis_opt(value)
            .single()
            .expect("valid timestamp");
        Id(at.format("%Y%m%d-%H%M%S-%3f").to_string())
    }

    pub fn dictated(&self, id: &Id, text: &str, audio: crate::mic::Audio16k) {
        if self.enabled.load(Ordering::Relaxed) {
            let _ = self.tx.send(Command::Dictated(
                Event::Dictated {
                    id: id.clone(),
                    at: Local::now().to_rfc3339(),
                    text: text.to_owned(),
                    audio: None,
                },
                audio,
            ));
        }
    }

    pub fn context(&self, id: &Id, terms: &[crate::context::Term], text: &str) {
        if self.enabled.load(Ordering::Relaxed) && !terms.is_empty() {
            let terms = terms
                .iter()
                .map(|term| ContextHit {
                    term: term.text.clone(),
                    source: term.source.to_owned(),
                    landed: crate::boost::occurrences(text, &term.text, false) > 0,
                })
                .collect();
            let _ = self.tx.send(Command::Event(Event::Context {
                id: id.clone(),
                terms,
            }));
        }
    }

    pub fn landed(
        &self,
        id: &Id,
        observation: &crate::target::Observation,
        landing: crate::paste::Landing,
    ) {
        if self.enabled.load(Ordering::Relaxed) {
            let _ = self.tx.send(Command::Event(Event::Landed {
                id: id.clone(),
                app: observation.app.clone(),
                bundle: observation.bundle.clone(),
                focus: observation.focus.as_str().to_owned(),
                landing: match landing {
                    crate::paste::Landing::Pasted => "pasted",
                    crate::paste::Landing::Held => "held",
                }
                .to_owned(),
            }));
        }
    }

    /// Teaches the learner from a transcript line fixed in the Library, the
    /// way the Fix dialog used to: the line's own dictation when the ledger
    /// has it, or a new entry for one it never saw.
    pub fn fixed(&self, at_ms: i64, original: &str, edited: &str) {
        if self.enabled.load(Ordering::Relaxed) && original.trim() != edited.trim() {
            let _ = self.tx.send(Command::Fixed(Fixed {
                at_ms,
                original: original.to_owned(),
                edited: edited.to_owned(),
            }));
        }
    }

    pub fn corrected(&self, id: &Id, text: &str, source: Source) {
        if self.enabled.load(Ordering::Relaxed) {
            let _ = self.tx.send(Command::Event(Event::Corrected {
                id: id.clone(),
                at: Local::now().to_rfc3339(),
                text: text.to_owned(),
                source,
            }));
        }
    }

    #[cfg(test)]
    pub fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Command::Flush(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

pub fn read(root: &Path) -> Vec<Event> {
    let Ok(file) = fs::File::open(root.join("events.jsonl")) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .filter_map(|line| serde_json::from_str(&line.ok()?).ok())
        .collect()
}

fn append(root: &Path, event: &Event) -> std::io::Result<()> {
    fs::create_dir_all(root)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("events.jsonl"))?;
    serde_json::to_writer(&mut file, event)?;
    file.write_all(b"\n")
}

fn compact(root: &Path, events: &mut Vec<Event>, keep: usize, slack: usize) -> std::io::Result<()> {
    let corrected: HashSet<Id> = events
        .iter()
        .filter_map(|event| match event {
            Event::Corrected { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let uncorrected: Vec<Id> = events
        .iter()
        .filter_map(|event| match event {
            Event::Dictated { id, .. } if !corrected.contains(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    if uncorrected.len() <= keep.saturating_add(slack) {
        return Ok(());
    }
    let retain: HashSet<Id> = uncorrected
        .iter()
        .rev()
        .take(keep)
        .cloned()
        .chain(corrected)
        .collect();
    let mut kept = Vec::with_capacity(events.len());
    for event in events.iter() {
        let id = match event {
            Event::Dictated { id, .. } | Event::Landed { id, .. } | Event::Corrected { id, .. } => {
                Some(id)
            }
            _ => None,
        };
        if id.is_some_and(|id| !retain.contains(id)) {
            continue;
        }
        let mut event = event.clone();
        if let Event::Dictated { audio, .. } = &mut event {
            if audio.as_ref().is_some_and(|path| !root.join(path).exists()) {
                *audio = None;
            }
        }
        kept.push(event);
    }
    let temp = root.join("events.jsonl.tmp");
    let mut file = fs::File::create(&temp)?;
    for event in &kept {
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    fs::rename(temp, root.join("events.jsonl"))?;
    *events = kept;
    let referenced: HashSet<PathBuf> = events
        .iter()
        .filter_map(|event| match event {
            Event::Dictated {
                audio: Some(path), ..
            } => Some(root.join(path)),
            _ => None,
        })
        .collect();
    if let Ok(files) = fs::read_dir(root.join("audio")) {
        for file in files.flatten() {
            if !referenced.contains(&file.path()) {
                let _ = fs::remove_file(file.path());
            }
        }
    }
    Ok(())
}

fn write_audio(path: &Path, audio: &crate::mic::Audio16k) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: crate::mic::RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &sample in audio.samples() {
        writer.write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)?;
    }
    writer.finalize()
}

fn write_corpus(root: &Path, corrected: &BTreeMap<Id, String>) -> std::io::Result<()> {
    fs::create_dir_all(root)?;
    let temp = root.join("corpus.tsv.tmp");
    let mut file = fs::File::create(&temp)?;
    writeln!(file, "# see-computer eval manifest: audio\tcorrected text")?;
    for (id, text) in corrected {
        let relative = format!("audio/{}.wav", id.0);
        if root.join(&relative).exists() {
            writeln!(
                file,
                "{}\t{}",
                relative,
                text.replace(['\t', '\n', '\r'], " ")
            )?;
        }
    }
    file.flush()?;
    fs::rename(temp, root.join("corpus.tsv"))
}

fn prune(root: &Path, keep: usize, slack: usize, corrected: &HashSet<Id>) {
    let Ok(entries) = fs::read_dir(root.join("audio")) else {
        return;
    };
    let all = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
        .collect::<Vec<_>>();
    if all.len() <= keep.saturating_add(slack) {
        return;
    }
    let mut files = all
        .into_iter()
        .filter(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| !corrected.contains(&Id(s.to_owned())))
        })
        .collect::<Vec<_>>();
    files.sort();
    let remove = files.len().saturating_sub(keep);
    for path in files.into_iter().take(remove) {
        let _ = fs::remove_file(path);
    }
}

const HEADER: &str = "# see.computer learned words\n#\n# Spellings you corrected in your own dictations, learned after two sightings\n# (or one fix in the Fix dialog). These are boosted like vocabulary.md, and a\n# line \"heard -> Word\" also rewrites what the model heard. Delete a line to\n# stop it; it will not be learned again.\n";

fn save_promotion(
    root: &Path,
    vocabulary: &Path,
    events: &mut Vec<Event>,
    promotion: crate::learn::Promotion,
) {
    let path = vocabulary.with_file_name("learned.md");
    let line = match &promotion.heard {
        Some(heard) => format!("- {heard} -> {}\n", promotion.term),
        None => format!("- {}\n", promotion.term),
    };
    if fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).is_err() {
        return;
    }
    let mut file = match OpenOptions::new().append(true).create(true).open(&path) {
        Ok(file) => file,
        Err(_) => return,
    };
    if file.metadata().is_ok_and(|m| m.len() == 0) && file.write_all(HEADER.as_bytes()).is_err() {
        return;
    }
    if fs::read(&path).is_ok_and(|bytes| !bytes.is_empty() && bytes.last() != Some(&b'\n'))
        && file.write_all(b"\n").is_err()
    {
        return;
    }
    if file.write_all(line.as_bytes()).is_err() {
        return;
    }
    let event = Event::Learned {
        at: Local::now().to_rfc3339(),
        term: promotion.term,
        heard: promotion.heard,
        from: promotion.from,
    };
    if append(root, &event).is_ok() {
        events.push(event);
    }
}

/// Saves what the evidence now supports and returns the terms it taught.
fn promote(
    root: &Path,
    vocabulary: &Path,
    words: &crate::learn::Words,
    events: &mut Vec<Event>,
) -> Vec<String> {
    let promotions = crate::learn::promotions(events, &known_terms(vocabulary), words);
    let mut names: Vec<_> = promotions.iter().map(|p| p.term.clone()).collect();
    names.dedup();
    for promotion in promotions {
        save_promotion(root, vocabulary, events, promotion);
    }
    names
}

/// The first spelling in a correction the learner can use, as the pill's
/// card shows it, or `None` when the fix teaches nothing: grammar, a lowered
/// capital, or a word the user told it to forget.
fn fix_notice(
    events: &[Event],
    id: &Id,
    edited: &str,
    vocabulary: &Path,
    words: &crate::learn::Words,
    learned: &[String],
) -> Option<crate::pill::Fix> {
    let pasted = events.iter().find_map(|event| match event {
        Event::Dictated { id: seen, text, .. } if seen == id => Some(text.as_str()),
        _ => None,
    })?;
    let known = known_terms(vocabulary);
    let waiting = crate::learn::waiting(events, &known, words);
    // A word this fix taught comes first: that is the news.
    crate::learn::swaps(pasted, edited)
        .into_iter()
        .filter(|swap| crate::learn::learnable(swap, words))
        .filter_map(|swap| {
            let learned = learned.contains(&swap.wrote);
            let open = waiting.iter().any(|w| w.term == swap.wrote) || known.contains(&swap.wrote);
            (learned || open).then_some(crate::pill::Fix {
                heard: swap.heard,
                wrote: swap.wrote,
                learned,
            })
        })
        .min_by_key(|fix| !fix.learned)
}

/// Every spelling either file holds, so a casing the user already has is
/// known even where the decoder prefers the other one.
fn known_terms(vocabulary: &Path) -> Vec<String> {
    let (vocabulary, learned) = crate::vocabulary::Files::beside(vocabulary)
        .read()
        .unwrap_or_default();
    vocabulary
        .lines()
        .chain(learned.lines())
        .filter_map(|line| match line {
            crate::vocabulary::Line::Active(entry) => Some(entry.term),
            _ => None,
        })
        .collect()
}

fn announce(pill: &Option<mpsc::Sender<PillEvent>>, names: &[String]) {
    let message = match names {
        [] => return,
        [one] => format!("Learned \"{one}\""),
        [one, two] => format!("Learned \"{one}\" and \"{two}\""),
        [one, rest @ ..] => format!("Learned \"{one}\" and {} more", rest.len()),
    };
    if let Some(pill) = pill {
        let _ = pill.send(PillEvent::Flash(Notice::Info(message)));
    }
}

fn update_learning(
    snapshot: &Arc<Mutex<Learning>>,
    events: &[Event],
    vocabulary: &Path,
    words: &crate::learn::Words,
) {
    let known = known_terms(vocabulary);
    // The latest decision about a spelling stands: a term renamed away and
    // back is learned again.
    let mut forgotten = HashSet::new();
    for event in events {
        match event {
            Event::Forgotten { term, .. } => {
                forgotten.insert(term.as_str());
            }
            Event::Learned { term, .. } => {
                forgotten.remove(term.as_str());
            }
            _ => {}
        }
    }
    let mut learning = Learning::default();
    for event in events.iter().rev() {
        match event {
            Event::Learned { term, .. }
                if !forgotten.contains(term.as_str()) && !learning.learned.contains(term) =>
            {
                learning.learned.push(term.clone())
            }
            _ => {}
        }
    }
    learning.waiting = crate::learn::waiting(events, &known, words);
    learning.heard = crate::learn::heard_forms(events, words);
    *snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = learning;
}

/// The ledger's own record of a fixed line: same text, dictated within two
/// minutes of when the history says it was. The closest one wins.
fn dictation_at(events: &[Event], fixed: &Fixed) -> Option<Id> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Dictated { id, at, text, .. } if text.trim() == fixed.original.trim() => {
                let at = chrono::DateTime::parse_from_rfc3339(at).ok()?;
                Some((id, (at.timestamp_millis() - fixed.at_ms).abs()))
            }
            _ => None,
        })
        .filter(|(_, gap)| *gap <= 120_000)
        .min_by_key(|(_, gap)| *gap)
        .map(|(id, _)| id.clone())
}

/// An id in the usual shape for a moment, moved on a millisecond at a time
/// past any id the log already holds.
fn unused_id(events: &[Event], at_ms: i64) -> Id {
    let taken: HashSet<&Id> = events
        .iter()
        .filter_map(|event| match event {
            Event::Dictated { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    (at_ms..)
        .filter_map(|ms| Local.timestamp_millis_opt(ms).single())
        .map(|at| Id(at.format("%Y%m%d-%H%M%S-%3f").to_string()))
        .find(|id| !taken.contains(id))
        .expect("some millisecond is free")
}

/// Removes every casing of `term` from `learned.md` and returns the
/// spellings removed, each of which the learner must not bring back.
fn remove_term(path: &Path, term: &str) -> std::io::Result<Vec<String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![term.to_owned()]),
        Err(e) => return Err(e),
    };
    let mut document = crate::vocabulary::Document::parse(&text);
    let mut spellings = vec![term.to_owned()];
    for line in document.lines() {
        if let crate::vocabulary::Line::Active(entry) = line {
            if crate::vocabulary::same(&entry.term, term) && !spellings.contains(&entry.term) {
                spellings.push(entry.term);
            }
        }
    }
    if !document.delete(term) {
        return Ok(spellings);
    }
    let temp = path.with_extension("md.tmp");
    fs::write(&temp, document.render())?;
    fs::rename(temp, path)?;
    Ok(spellings)
}

/// Learning a waiting word and ignoring one change only the log; every other
/// edit changes the files first and then records what the learner must know.
fn apply_edit(
    root: &Path,
    vocabulary: &Path,
    words: &crate::learn::Words,
    events: &mut Vec<Event>,
    pill: &Option<mpsc::Sender<PillEvent>>,
    edit: crate::vocabulary::Edit,
) -> Result<(), String> {
    use crate::vocabulary::{Edit, Files, Note};
    const GONE: &str = "That word is no longer waiting, so the list has been reloaded.";
    let now = || Local::now().to_rfc3339();
    let record = |events: &mut Vec<Event>, event: Event| {
        append(root, &event).map_err(|error| format!("Unable to save: {error}."))?;
        events.push(event);
        Ok::<(), String>(())
    };
    match edit {
        Edit::Learn { term } => {
            let known = known_terms(vocabulary);
            let waiting = crate::learn::waiting(events, &known, words)
                .into_iter()
                .find(|waiting| waiting.term == term)
                .ok_or(GONE)?;
            let promotion = crate::learn::Promotion {
                term: waiting.term,
                heard: waiting.heard.into_iter().next(),
                from: waiting.from,
            };
            announce(pill, std::slice::from_ref(&promotion.term));
            save_promotion(root, vocabulary, events, promotion);
            Ok(())
        }
        Edit::Ignore { term } => {
            let known = known_terms(vocabulary);
            if !crate::learn::waiting(events, &known, words)
                .iter()
                .any(|waiting| waiting.term == term)
            {
                return Err(GONE.to_owned());
            }
            record(events, Event::Forgotten { at: now(), term })
        }
        edit => {
            for note in Files::beside(vocabulary).edit(&edit)? {
                let event = match note {
                    Note::Forgot(term) => Event::Forgotten { at: now(), term },
                    Note::Learned(term) => Event::Learned {
                        at: now(),
                        term,
                        heard: None,
                        from: Vec::new(),
                    },
                };
                record(events, event)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
impl Id {
    pub(crate) fn test(value: &str) -> Id {
        Id(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(keep: usize) -> (Ledger, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "see-ledger-{}-{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap()
        ));
        let ledger = Ledger::start_at(
            root.clone(),
            Arc::new(AtomicBool::new(true)),
            keep,
            Some(root.join("vocabulary.md")),
            None,
        );
        ledger.flush();
        (ledger, root)
    }

    #[test]
    fn dictated_audio_and_events_round_trip() {
        let (ledger, root) = fixture(500);
        let id = ledger.mint();
        ledger.dictated(
            &id,
            "Tauri",
            crate::mic::Audio16k::from_samples(vec![0.1; 320]),
        );
        let observation = crate::target::Observation {
            pid: 1,
            app: "Notes".into(),
            bundle: "com.apple.Notes".into(),
            focus: crate::target::Focus::Editable,
            element: None,
        };
        ledger.landed(&id, &observation, crate::paste::Landing::Pasted);
        ledger.corrected(&id, "Tauri!", Source::ReadBack);
        ledger.flush();
        let events = read(&root);
        assert_eq!(events.len(), 3);
        assert!(
            matches!(&events[0], Event::Dictated { id: actual, text, audio: Some(path), .. } if actual == &id && text == "Tauri" && path == &format!("audio/{}.wav", id.0))
        );
        assert_eq!(
            crate::mic::Audio16k::from_wav(&root.join(format!("audio/{}.wav", id.0)))
                .unwrap()
                .samples()
                .len(),
            320
        );
        assert!(
            matches!(&events[1], Event::Landed { app, bundle, focus, landing, .. } if app == "Notes" && bundle == "com.apple.Notes" && focus == "editable" && landing == "pasted")
        );
        assert!(
            matches!(&events[2], Event::Corrected { text, source: Source::ReadBack, .. } if text == "Tauri!")
        );
    }

    #[test]
    fn context_event_records_only_terms_and_landing() {
        let (ledger, root) = fixture(500);
        let id = ledger.mint();
        ledger.context(
            &id,
            &[crate::context::Term {
                text: "Zorvia".into(),
                source: "screen",
            }],
            "Use Zorvia today",
        );
        ledger.flush();
        let events = read(&root);
        assert!(
            matches!(&events[0], Event::Context { terms, .. } if terms == &[ContextHit {
                term: "Zorvia".into(), source: "screen".into(), landed: true
            }])
        );
        assert!(!std::fs::read_to_string(root.join("events.jsonl"))
            .unwrap()
            .contains("Use Zorvia today"));
        drop(ledger);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn literal_json_is_stable() {
        let line = r#"{"event":"corrected","id":"20261014-231502-123","at":"2026-10-14T23:15:02+02:00","text":"Tauri","source":"read-back"}"#;
        assert_eq!(
            serde_json::from_str::<Event>(line).unwrap(),
            Event::Corrected {
                id: Id("20261014-231502-123".into()),
                at: "2026-10-14T23:15:02+02:00".into(),
                text: "Tauri".into(),
                source: Source::ReadBack,
            }
        );
    }

    #[test]
    fn latest_correction_makes_eval_manifest() {
        let (ledger, root) = fixture(500);
        let id = ledger.mint();
        ledger.dictated(&id, "first", crate::mic::Audio16k::silence(0.1));
        ledger.corrected(&id, "old", Source::Manual);
        ledger.corrected(&id, "new\tword\nline", Source::Manual);
        ledger.flush();
        let manifest = fs::read_to_string(root.join("corpus.tsv")).unwrap();
        assert_eq!(manifest.lines().count(), 2);
        let (clips, errors) = crate::eval::parse_manifest(&root.join("corpus.tsv"), &manifest);
        assert!(errors.is_empty());
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].reference, "new word line");
        assert!(clips[0].path.exists());
    }

    #[test]
    fn pruning_keeps_corrected_and_newest_audio() {
        let (ledger, root) = fixture(2);
        let mut ids = Vec::new();
        for _ in 0..5 {
            let id = ledger.mint();
            ledger.dictated(&id, "text", crate::mic::Audio16k::silence(0.01));
            ids.push(id);
            if ids.len() == 1 {
                ledger.corrected(&ids[0], "correct", Source::Manual);
            }
        }
        ledger.flush();
        let protected = HashSet::from([ids[0].clone()]);
        prune(&root, 2, 0, &protected);
        for index in [0, 3, 4] {
            assert!(root.join(format!("audio/{}.wav", ids[index].0)).exists());
        }
        for index in [1, 2] {
            assert!(!root.join(format!("audio/{}.wav", ids[index].0)).exists());
        }
    }

    #[test]
    fn compaction_keeps_corrected_dictations_and_referenced_audio() {
        let (ledger, root) = fixture(2);
        let mut ids = Vec::new();
        for index in 0..5 {
            let id = ledger.mint();
            ledger.dictated(&id, "word", crate::mic::Audio16k::silence(0.01));
            if index == 0 {
                ledger.corrected(&id, "Word", Source::Manual);
            }
            ids.push(id);
        }
        ledger.flush();
        let mut events = read(&root);
        compact(&root, &mut events, 2, 0).unwrap();
        assert_eq!(read(&root), events);
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Corrected { id, .. } if id == &ids[0])));
        for (index, id) in ids.iter().enumerate() {
            let retained = [0, 3, 4].contains(&index);
            assert_eq!(
                events
                    .iter()
                    .any(|e| matches!(e, Event::Dictated { id: found, .. } if found == id)),
                retained
            );
            assert_eq!(root.join(format!("audio/{}.wav", id.0)).exists(), retained);
        }
        assert!(events.iter().all(|e| match e {
            Event::Dictated {
                audio: Some(path), ..
            } => root.join(path).exists(),
            _ => true,
        }));
    }

    #[test]
    fn learns_and_forgets_a_repeated_correction() {
        let (ledger, root) = fixture(500);
        for _ in 0..2 {
            let id = ledger.mint();
            ledger.dictated(&id, "superbase", crate::mic::Audio16k::silence(0.01));
            ledger.corrected(&id, "Supabase", Source::ReadBack);
        }
        ledger.flush();
        assert_eq!(
            read(&root)
                .iter()
                .filter(|e| matches!(e, Event::Learned { .. }))
                .count(),
            1
        );
        let lines = fs::read_to_string(root.join("learned.md")).unwrap();
        assert_eq!(
            lines
                .lines()
                .filter(|l| !l.starts_with('#'))
                .collect::<Vec<_>>(),
            ["- superbase -> Supabase"]
        );
        assert_eq!(ledger.learning().learned, ["Supabase"]);
        ledger.forget("Supabase");
        ledger.flush();
        assert!(fs::read_to_string(root.join("learned.md"))
            .unwrap()
            .lines()
            .all(|l| l.starts_with('#')));
        assert!(ledger.learning().learned.is_empty());
    }

    #[test]
    fn a_line_fixed_in_the_library_teaches_like_the_fix_dialog() {
        let (ledger, root) = fixture(500);
        let id = ledger.mint();
        ledger.dictated(&id, "we use superbase", crate::mic::Audio16k::silence(0.01));
        ledger.flush();
        let at_ms = Local::now().timestamp_millis();
        ledger.fixed(at_ms, "we use superbase", "we use Supabase");
        ledger.fixed(
            at_ms - 86_400_000,
            "ship with fastlain",
            "ship with Fastlane",
        );
        ledger.flush();
        let events = read(&root);
        assert!(
            events.iter().any(|e| matches!(e,
                Event::Corrected { id: fixed, source: Source::Manual, .. } if *fixed == id)),
            "a line the ledger dictated is corrected under its own id"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::Dictated { audio: None, text, .. } if text == "ship with fastlain"))
                .count(),
            1,
            "a line it never saw gets an entry of its own"
        );
        let learned = fs::read_to_string(root.join("learned.md")).unwrap();
        assert!(learned.contains("- superbase -> Supabase\n"), "{learned}");
        assert!(learned.contains("- fastlain -> Fastlane\n"), "{learned}");
    }

    #[test]
    fn promotion_appends_after_unterminated_line() {
        let (ledger, root) = fixture(500);
        fs::write(root.join("learned.md"), "- Tauri").unwrap();
        let id = ledger.mint();
        ledger.dictated(&id, "superbase", crate::mic::Audio16k::silence(0.01));
        ledger.corrected(&id, "Supabase", Source::Manual);
        ledger.flush();
        assert_eq!(
            fs::read_to_string(root.join("learned.md")).unwrap(),
            "- Tauri\n- superbase -> Supabase\n"
        );
    }

    #[test]
    fn disabled_writes_nothing_and_mint_is_unique() {
        let enabled = Arc::new(AtomicBool::new(false));
        let root = std::env::temp_dir().join(format!(
            "see-ledger-off-{}",
            Local::now().timestamp_nanos_opt().unwrap()
        ));
        let ledger = Ledger::start_at(
            root.clone(),
            enabled,
            2,
            Some(root.join("vocabulary.md")),
            None,
        );
        let mut ids = HashSet::new();
        for _ in 0..1000 {
            ids.insert(ledger.mint());
        }
        assert_eq!(ids.len(), 1000);
        let id = ledger.mint();
        ledger.dictated(&id, "ignored", crate::mic::Audio16k::silence(0.1));
        ledger.corrected(&id, "ignored", Source::Manual);
        ledger.flush();
        assert!(read(&root).is_empty());
    }

    #[test]
    fn a_read_back_fix_shows_what_was_seen_and_then_what_was_learned() {
        let root = std::env::temp_dir().join(format!(
            "see-ledger-fix-{}-{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap()
        ));
        let (pill, notices) = mpsc::channel();
        let ledger = Ledger::start_at(
            root.clone(),
            Arc::new(AtomicBool::new(true)),
            500,
            Some(root.join("vocabulary.md")),
            Some(pill),
        );
        for (said, fixed) in [
            (
                "I sent the file to Larston yesterday.",
                "I sent the file to Larsten yesterday.",
            ),
            ("I want to go home.", "I want too go home."),
            ("Ask Larston about it.", "Ask Larsten about it."),
        ] {
            let id = ledger.mint();
            ledger.dictated(
                &id,
                said,
                crate::mic::Audio16k::from_samples(vec![0.0; 160]),
            );
            ledger.corrected(&id, fixed, Source::ReadBack);
        }
        ledger.flush();
        let shown: Vec<_> = notices
            .try_iter()
            .map(|event| match event {
                PillEvent::Fixed(fix) => format!("{} -> {} {}", fix.heard, fix.wrote, fix.learned),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            shown,
            vec!["Larston -> Larsten false", "Larston -> Larsten true"]
        );
        let _ = fs::remove_dir_all(root);
    }
}
