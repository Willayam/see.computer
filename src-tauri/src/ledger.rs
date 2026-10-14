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

enum Command {
    Dictated(Event, crate::mic::Audio16k),
    Event(Event),
    Flush(mpsc::Sender<()>),
    Forget(String),
    Teach(String),
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
    pub last: Option<(Id, String)>,
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
                            update_learning(&snapshot, &events, &vocabulary, &words);
                        }
                        let protected = corrected.keys().cloned().collect();
                        prune(&root, keep_audio, 50, &protected);
                    }
                    Command::Event(event) => {
                        if append(&root, &event).is_ok() {
                            if let Event::Corrected { id, text, .. } = &event {
                                corrected.insert(id.clone(), text.clone());
                                let _ = write_corpus(&root, &corrected);
                            }
                            let correct = matches!(event, Event::Corrected { .. });
                            events.push(event);
                            if correct {
                                promote(&root, &vocabulary, &words, &mut events, &pill);
                            }
                            update_learning(&snapshot, &events, &vocabulary, &words);
                        }
                    }
                    Command::Forget(term) => {
                        let event = Event::Forgotten {
                            at: Local::now().to_rfc3339(),
                            term: term.clone(),
                        };
                        if append(&root, &event).is_ok() {
                            events.push(event);
                            let _ = remove_term(&vocabulary.with_file_name("learned.md"), &term);
                            update_learning(&snapshot, &events, &vocabulary, &words);
                        }
                    }
                    Command::Teach(term) => {
                        let known = known_terms(&vocabulary);
                        let taught = crate::learn::waiting(&events, &known, &words)
                            .into_iter()
                            .find(|waiting| waiting.term == term);
                        if let Some(waiting) = taught {
                            let promotion = crate::learn::Promotion {
                                term: waiting.term,
                                heard: waiting.heard.into_iter().next(),
                                from: waiting.from,
                            };
                            announce(&pill, std::slice::from_ref(&promotion.term));
                            save_promotion(&root, &vocabulary, &mut events, promotion);
                            update_learning(&snapshot, &events, &vocabulary, &words);
                        }
                    }
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
    pub fn forget(&self, term: &str) {
        let _ = self.tx.send(Command::Forget(term.to_owned()));
    }
    pub fn teach(&self, term: &str) {
        let _ = self.tx.send(Command::Teach(term.to_owned()));
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

fn promote(
    root: &Path,
    vocabulary: &Path,
    words: &crate::learn::Words,
    events: &mut Vec<Event>,
    pill: &Option<mpsc::Sender<PillEvent>>,
) {
    let promotions = crate::learn::promotions(events, &known_terms(vocabulary), words);
    let mut names: Vec<_> = promotions.iter().map(|p| p.term.clone()).collect();
    names.dedup();
    for promotion in promotions {
        save_promotion(root, vocabulary, events, promotion);
    }
    announce(pill, &names);
}

fn known_terms(vocabulary: &Path) -> Vec<String> {
    crate::boost::Lexicon::load(vocabulary.to_owned())
        .terms()
        .iter()
        .map(|term| term.text.clone())
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
    let forgotten: HashSet<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Forgotten { term, .. } => Some(term.as_str()),
            _ => None,
        })
        .collect();
    let mut learning = Learning::default();
    for event in events.iter().rev() {
        match event {
            Event::Dictated { id, text, .. } if learning.last.is_none() => {
                learning.last = Some((id.clone(), text.clone()))
            }
            Event::Learned { term, .. }
                if !forgotten.contains(term.as_str()) && !learning.learned.contains(term) =>
            {
                learning.learned.push(term.clone())
            }
            _ => {}
        }
    }
    learning.waiting = crate::learn::waiting(events, &known, words);
    *snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = learning;
}

fn remove_term(path: &Path, term: &str) -> std::io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let kept = text
        .lines()
        .filter(|line| {
            let body = line.trim().strip_prefix("- ").unwrap_or(line.trim());
            let body = body.split_once('\t').map_or(body, |(body, _)| body);
            let body = body.split_once(" -> ").map_or(body, |(_, term)| term);
            body != term
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let temp = path.with_extension("md.tmp");
    fs::write(&temp, kept)?;
    fs::rename(temp, path)
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
            app: "Notes".into(),
            bundle: "com.apple.Notes".into(),
            focus: crate::target::Focus::Editable,
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
}
