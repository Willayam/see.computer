use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct History {
    root: Arc<Mutex<PathBuf>>,
    enabled: Arc<AtomicBool>,
    tx: Sender<Request>,
    words: Arc<Mutex<Option<crate::tally::Tally>>>,
}

/// The history thread is the only writer of day files, so a fix made in the
/// Library cannot race the append of the next dictation.
enum Request {
    Load,
    Refresh(Sender<()>),
    Append(String, Option<std::time::Duration>, Option<PathBuf>),
    Replace(Fix, Sender<Result<(), String>>),
}

/// One dictation's text as the Library showed it, and what the user changed
/// it to.
pub struct Fix {
    pub file: String,
    pub start_ms: i64,
    pub original: String,
    pub edited: String,
}

pub struct Entry {
    pub start_ms: i64,
    pub at: NaiveDateTime,
    pub text: String,
}

impl History {
    pub fn start(enabled: bool, pill: Option<Sender<crate::pill::PillEvent>>) -> History {
        let preferred = crate::paths::documents();
        let fallback = crate::paths::history_fallback();
        let seam = std::env::var_os("SEE_COMPUTER_HISTORY_DIR").map(PathBuf::from);
        let speed = crate::paths::speed_log();
        let ledger = std::env::var_os("SEE_COMPUTER_LEDGER_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(crate::paths::ledger);
        Self::start_at(
            enabled, pill, preferred, fallback, seam, speed, ledger, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_at(
        enabled: bool,
        pill: Option<Sender<crate::pill::PillEvent>>,
        preferred: PathBuf,
        fallback: PathBuf,
        seam: Option<PathBuf>,
        speed: PathBuf,
        ledger: PathBuf,
        trails: Option<Vec<PathBuf>>,
    ) -> History {
        let initial = seam.clone().unwrap_or_else(|| preferred.clone());
        let root = Arc::new(Mutex::new(initial));
        let enabled = Arc::new(AtomicBool::new(enabled));
        let (tx, rx) = mpsc::channel::<Request>();
        let writer_root = root.clone();
        let words = Arc::new(Mutex::new(None));
        let writer_words = words.clone();
        let writer_enabled = enabled.clone();
        let reload = tx.clone();
        crate::qos::spawn("see-history", crate::qos::Class::Upkeep, move || {
            let mut backfill_due = !speed.exists();
            let cutoff = Local::now().timestamp_millis();
            let can_fallback = seam.is_none();
            let live = match &seam {
                Some(path) => {
                    let _ = std::fs::create_dir_all(path).and_then(|()| touch_today(path));
                    path.clone()
                }
                None => resolve_root(&preferred, &fallback),
            };
            set_root(&writer_root, live);
            let waiter = Arc::new(AtomicBool::new(false));
            let mut counts = crate::tally::DayCounts::new();
            let mut load = || {
                if !writer_enabled.load(Ordering::Relaxed) {
                    return;
                }
                let live = locked_root(&writer_root);
                let mut tally = crate::tally::Tally::read_cached(&live, &speed, &mut counts);
                if !tally.waiting.is_empty() {
                    // The backfill runs once, so it waits until every day file is
                    // local; otherwise the evicted days would never be timed.
                    let waiting = tally.waiting.clone();
                    if !waiter.swap(true, Ordering::Relaxed) {
                        let waiter = waiter.clone();
                        let reload = reload.clone();
                        std::thread::spawn(move || {
                            crate::library::download(&waiting);
                            let deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(600);
                            let readable = || {
                                waiting.iter().all(|path| {
                                    matches!(
                                        crate::shadow::read(path),
                                        crate::shadow::Text::Current(_)
                                            | crate::shadow::Text::Stale(_)
                                    )
                                })
                            };
                            let mut ready = readable();
                            while !ready && std::time::Instant::now() < deadline {
                                std::thread::sleep(std::time::Duration::from_millis(500));
                                ready = readable();
                            }
                            waiter.store(false, Ordering::Relaxed);
                            if ready {
                                let _ = reload.send(Request::Load);
                            }
                        });
                    }
                } else if backfill_due {
                    let trails = trails.clone().unwrap_or_else(|| {
                        if let Some(path) = std::env::var_os("SEE_COMPUTER_STATE_LOG") {
                            vec![PathBuf::from(path)]
                        } else {
                            std::fs::read_dir(crate::paths::app_support())
                                .into_iter()
                                .flatten()
                                .flatten()
                                .filter(|entry| {
                                    entry.file_name() == "events.tsv"
                                        || entry
                                            .file_name()
                                            .to_string_lossy()
                                            .starts_with("events.tsv.bak-")
                                })
                                .map(|entry| entry.path())
                                .collect()
                        }
                    });
                    if crate::tally::backfill(&live, &speed, &ledger, &trails, cutoff).is_ok() {
                        backfill_due = false;
                    }
                    tally = crate::tally::Tally::read_cached(&live, &speed, &mut counts);
                }
                let mut slot = writer_words
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *slot = writer_enabled.load(Ordering::Relaxed).then_some(tally);
            };
            load();
            while let Ok(write) = rx.recv() {
                let (text, spoken, saved_audio) = match write {
                    Request::Load => {
                        load();
                        crate::library::changed();
                        continue;
                    }
                    Request::Refresh(reply) => {
                        load();
                        let _ = reply.send(());
                        continue;
                    }
                    Request::Append(text, spoken, saved_audio) => (text, spoken, saved_audio),
                    Request::Replace(fix, reply) => {
                        let result = replace_entry(&locked_root(&writer_root), &fix);
                        if result.is_ok() {
                            load();
                            crate::library::changed();
                        }
                        let _ = reply.send(result);
                        continue;
                    }
                };
                if !writer_enabled.load(Ordering::Relaxed) {
                    continue;
                }
                let at = Local::now();
                let live = locked_root(&writer_root);
                let filed = match append_entry(&live, at, &text) {
                    Err(error) if can_fallback && error.raw_os_error() == Some(libc::EPERM) => {
                        let _ = std::fs::create_dir_all(&fallback);
                        set_root(&writer_root, fallback.clone());
                        append_entry(&fallback, at, &text)
                    }
                    result => result,
                };
                if let Ok(file) = filed {
                    if let Some(path) = saved_audio {
                        // Keep the retry WAV if the history file cannot be flushed to disk.
                        if file.sync_data().is_ok() {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                    if let Some(spoken) = spoken {
                        let _ =
                            crate::tally::log_speed(&speed, at, spoken, crate::tally::words(&text));
                    }
                    if let Some(tally) = writer_words
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_mut()
                    {
                        let before = crate::tally::level(tally.total().words).index;
                        let words = crate::tally::words(&text);
                        *tally.days.entry(at.date_naive()).or_default() += crate::tally::Count {
                            words,
                            dictations: 1,
                            timed_words: spoken.map_or(0, |_| words),
                            spoken_ms: spoken.map_or(0, |d| d.as_millis() as u64),
                        };
                        let total = tally.total();
                        let level = crate::tally::level(total.words);
                        if tally.waiting.is_empty()
                            && level.index > before
                            && writer_enabled.load(Ordering::Relaxed)
                        {
                            if let Some(pill) = &pill {
                                let _ = pill.send(crate::pill::PillEvent::LevelUp(
                                    crate::pill::LevelUp {
                                        index: level.index,
                                        name: level.name.unwrap_or_default(),
                                        at: level.at,
                                        total: total.words,
                                        times_typing: total
                                            .wpm()
                                            .map(|n| (n as f64 / 4.0).round() / 10.0),
                                    },
                                ));
                            }
                        }
                    }
                    crate::library::changed();
                }
            }
        });
        History {
            root,
            enabled,
            tx,
            words,
        }
    }

    pub fn record(&self, text: &str, spoken: Option<std::time::Duration>) {
        self.record_recovery(text, spoken, None);
    }

    pub fn record_recovery(
        &self,
        text: &str,
        spoken: Option<std::time::Duration>,
        saved_audio: Option<PathBuf>,
    ) {
        if self.enabled() {
            let _ = self
                .tx
                .send(Request::Append(text.to_owned(), spoken, saved_audio));
        }
    }

    /// Rewrites one entry's text in its day file. The reply says whether the
    /// entry was still there as the Library showed it.
    pub fn replace(&self, fix: Fix) -> Receiver<Result<(), String>> {
        let (reply, outcome) = mpsc::channel();
        if let Err(mpsc::SendError(Request::Replace(_, reply))) =
            self.tx.send(Request::Replace(fix, reply))
        {
            let _ = reply.send(Err(
                "Unable to save. Quit and reopen see.computer, then try again.".to_owned(),
            ));
        }
        outcome
    }

    pub fn recent(&self, limit: usize) -> Vec<Entry> {
        read_recent(&self.root(), limit)
    }

    pub fn root(&self) -> PathBuf {
        locked_root(&self.root)
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn enabled_flag(&self) -> Arc<AtomicBool> {
        self.enabled.clone()
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        if on {
            let _ = self.tx.send(Request::Load);
        } else {
            *self
                .words
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            crate::library::changed();
        }
    }

    pub fn words(&self) -> Option<crate::tally::Tally> {
        if !self.enabled() {
            return None;
        }
        self.words
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn refresh_words(&self) -> Option<crate::tally::Tally> {
        if !self.enabled() {
            return None;
        }
        let (reply, result) = mpsc::channel();
        self.tx.send(Request::Refresh(reply)).ok()?;
        result.recv().ok()?;
        self.words()
    }

    #[cfg(test)]
    pub fn off() -> History {
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        History {
            root: Arc::new(Mutex::new(
                std::env::temp_dir().join(format!("see-history-off-{unique}")),
            )),
            enabled: Arc::new(AtomicBool::new(false)),
            tx,
            words: Arc::new(Mutex::new(None)),
        }
    }
}

fn resolve_root(preferred: &Path, fallback: &Path) -> PathBuf {
    let preferred_ready = std::fs::create_dir_all(preferred)
        .and_then(|()| heal_forward(preferred, fallback))
        .and_then(|()| touch_today(preferred));
    if preferred_ready.is_ok() {
        return preferred.to_path_buf();
    }
    let _ = std::fs::create_dir_all(fallback).and_then(|()| touch_today(fallback));
    fallback.to_path_buf()
}

fn append_entry(root: &Path, at: DateTime<Local>, text: &str) -> io::Result<std::fs::File> {
    let path = root.join(format!("{}.md", at.format("%Y-%m-%d")));
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    write!(file, "## {}\n\n{text}\n\n", at.format("%H:%M:%S %:z"))?;
    Ok(file)
}

fn read_recent(root: &Path, limit: usize) -> Vec<Entry> {
    if limit == 0 {
        return Vec::new();
    }
    let days = day_files(root);
    let mut entries = Vec::new();
    for (date, path) in days.into_iter().take(14) {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        entries.extend(parse_day(date, &content));
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.start_ms));
    entries.truncate(limit);
    entries
}

pub(crate) fn day_file_date(stem: &str) -> Option<NaiveDate> {
    let date = if let Some((date, copy)) = stem.split_once(' ') {
        if copy.parse::<u32>().ok()? < 2 || copy.contains(' ') {
            return None;
        }
        date
    } else {
        stem
    };
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

pub(crate) fn day_files(root: &Path) -> Vec<(NaiveDate, PathBuf)> {
    let mut days = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "md") {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(day_file_date)
                    .map(|date| (date, path))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    days.sort_by_key(|(date, _)| std::cmp::Reverse(*date));
    days
}

fn replace_entry(root: &Path, fix: &Fix) -> Result<(), String> {
    if day_file_date(&fix.file).is_none() {
        return Err("That dictation has no valid day file.".to_owned());
    }
    let path = root.join(format!("{}.md", fix.file));
    let failed = |error: io::Error| format!("Unable to save: {error}.");
    let before = std::fs::read_to_string(&path).map_err(failed)?;
    let after = replaced(&before, fix).ok_or_else(|| {
        "That dictation changed on disk, so the Library has been reloaded.".to_owned()
    })?;
    let temp = path.with_extension("md.tmp");
    std::fs::write(&temp, after).map_err(failed)?;
    if std::fs::read_to_string(&path).ok().as_deref() != Some(before.as_str()) {
        let _ = std::fs::remove_file(&temp);
        return Err("The day's history changed while saving. Try again.".to_owned());
    }
    std::fs::rename(temp, path).map_err(failed)
}

/// The day file with one entry's body swapped, or `None` when no entry at
/// that time still reads as `original`. Every other byte is kept.
fn replaced(content: &str, fix: &Fix) -> Option<String> {
    let date = day_file_date(&fix.file)?;
    for block in day_blocks(date, content) {
        let body = &content[block.body.clone()];
        if block.at.timestamp_millis() == fix.start_ms && body.trim() == fix.original.trim() {
            let start = block.body.start;
            let end = start + body.trim_end().len();
            return Some(format!(
                "{}{}{}",
                &content[..start],
                fix.edited.trim(),
                &content[end..]
            ));
        }
    }
    None
}

struct Block {
    at: DateTime<chrono::FixedOffset>,
    body: std::ops::Range<usize>,
}

fn day_blocks(date: NaiveDate, content: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut pos = 0;
    for line in content.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let Some(time) = line.trim_end_matches(['\n', '\r']).strip_prefix("## ") else {
            continue;
        };
        let at = if time.contains(' ') {
            DateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S %:z").ok()
        } else {
            NaiveTime::parse_from_str(time, "%H:%M:%S")
                .ok()
                .and_then(|t| {
                    match Local.from_local_datetime(&date.and_time(t)) {
                        chrono::LocalResult::Ambiguous(a, b) => {
                            let (first, second) = (a.min(b), a.max(b));
                            Some(if blocks.last().is_some_and(|b| b.at >= first) {
                                second
                            } else {
                                first
                            })
                        }
                        result => result.single(),
                    }
                    .map(|time| time.fixed_offset())
                })
        };
        if let Some(at) = at {
            if let Some(last) = blocks.last_mut() {
                last.body.end = start;
            }
            let body_start = pos + usize::from(content[pos..].starts_with('\n'));
            blocks.push(Block {
                at,
                body: body_start..content.len(),
            });
        }
    }
    blocks
}

pub(crate) fn day_entries(date: NaiveDate, content: &str) -> Vec<Entry> {
    day_blocks(date, content)
        .into_iter()
        .map(|b| {
            let body = &content[b.body];
            Entry {
                at: b.at.naive_local(),
                start_ms: b.at.timestamp_millis(),
                text: body.strip_suffix("\n\n").unwrap_or(body).to_owned(),
            }
        })
        .collect()
}

pub(crate) fn parse_day(date: NaiveDate, content: &str) -> Vec<Entry> {
    let mut entries = day_entries(date, content);
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.start_ms));
    entries
}

fn heal_forward(preferred: &Path, fallback: &Path) -> io::Result<()> {
    let entries = match std::fs::read_dir(fallback) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let source = entry?.path();
        let valid_day = source
            .extension()
            .is_some_and(|extension| extension == "md")
            && source
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| NaiveDate::parse_from_str(stem, "%Y-%m-%d").is_ok());
        if !valid_day {
            continue;
        }
        let destination = preferred.join(source.file_name().unwrap_or_default());
        if destination.exists() {
            let fallback_content = std::fs::read(&source)?;
            let mut destination_file = OpenOptions::new().append(true).open(&destination)?;
            if destination_file.metadata()?.len() > 0 {
                let content = std::fs::read(&destination)?;
                if !content.ends_with(b"\n") {
                    destination_file.write_all(b"\n")?;
                }
            }
            destination_file.write_all(&fallback_content)?;
            std::fs::remove_file(source)?;
        } else {
            std::fs::rename(source, destination)?;
        }
    }
    Ok(())
}

fn touch_today(root: &Path) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(format!("{}.md", Local::now().format("%Y-%m-%d"))))
        .map(|_| ())
}

fn locked_root(root: &Mutex<PathBuf>) -> PathBuf {
    root.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn set_root(root: &Mutex<PathBuf>, path: PathBuf) {
    *root
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("see-history-test-{unique}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn local(date: &str, time: &str) -> DateTime<Local> {
        let naive =
            NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S").unwrap();
        Local.from_local_datetime(&naive).single().unwrap()
    }

    #[test]
    fn late_audio_survives_a_failed_history_write_and_is_removed_after_a_success() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("history");
        let history = History::start_at(
            true,
            None,
            root.clone(),
            root.clone(),
            Some(root.clone()),
            dir.path().join("speed"),
            dir.path().join("ledger"),
            Some(vec![]),
        );
        history.refresh_words();
        let audio = dir.path().join("take.wav");
        fs::write(&audio, b"test audio").unwrap();
        let day = root.join(format!("{}.md", Local::now().format("%Y-%m-%d")));
        if day.is_file() {
            fs::remove_file(&day).unwrap()
        }
        fs::create_dir(&day).unwrap();
        history.record_recovery("recovered", None, Some(audio.clone()));
        history.refresh_words();
        assert!(audio.exists());
        fs::remove_dir(day).unwrap();
        history.record_recovery("recovered", None, Some(audio.clone()));
        history.refresh_words();
        assert!(!audio.exists());
        assert_eq!(history.recent(1)[0].text, "recovered");
    }

    #[test]
    fn waiting_day_prevents_level_up() {
        let root = temp_dir();
        append_entry(&root, Local::now(), &vec!["word"; 999].join(" ")).unwrap();
        fs::create_dir(root.join("2020-01-01.md")).unwrap();
        let (tx, rx) = mpsc::channel();
        let history = History::start_at(
            true,
            Some(tx),
            root.clone(),
            root.clone(),
            Some(root.clone()),
            root.join("speed.tsv"),
            root.join("ledger"),
            Some(vec![]),
        );
        let tally = history.refresh_words().unwrap();
        assert_eq!(tally.total().words, 999);
        assert_eq!(tally.waiting, vec![root.join("2020-01-01.md")]);
        history.record("one two", None);
        assert_eq!(history.refresh_words().unwrap().total().words, 1001);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn live_tally_emits_one_level_up_and_loads_when_enabled() {
        let root = temp_dir();
        append_entry(&root, Local::now(), &vec!["word"; 999].join(" ")).unwrap();
        let (tx, rx) = mpsc::channel();
        let history = History::start_at(
            false,
            Some(tx),
            root.clone(),
            root.clone(),
            Some(root.clone()),
            root.join("speed.tsv"),
            root.join("ledger"),
            Some(vec![]),
        );
        assert!(history.words().is_none());
        assert!(!root.join("speed.tsv").exists());
        history.set_enabled(true);
        assert_eq!(history.refresh_words().unwrap().total().words, 999);
        assert!(rx.try_recv().is_err());
        history.record(
            "one two three four five",
            Some(std::time::Duration::from_secs(60)),
        );
        match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
            crate::pill::PillEvent::LevelUp(level) => {
                assert_eq!(level.index, 1);
                assert_eq!(level.name, "Letter");
                assert_eq!(level.at, 1000);
                assert_eq!(level.total, 1004);
            }
            event => panic!("unexpected event {event:?}"),
        }
        history.record("six seven", None);
        assert_eq!(history.refresh_words().unwrap().total().words, 1006);
        assert!(rx.try_recv().is_err());
        assert_eq!(history.words().unwrap().total().dictations, 3);
        let entry = history
            .recent(3)
            .into_iter()
            .find(|entry| entry.text == "six seven")
            .unwrap();
        history
            .replace(Fix {
                file: entry.at.date().to_string(),
                start_ms: entry.start_ms,
                original: entry.text,
                edited: vec!["word"; 5000].join(" "),
            })
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(history.words().unwrap().total().words, 6004);
        assert!(rx.try_recv().is_err());
        history.set_enabled(false);
        assert!(history.words().is_none());
        drop(history);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn appends_and_reads_newest_first() {
        let root = temp_dir();
        append_entry(&root, local("2026-08-20", "09:10:11"), "första texten").unwrap();
        append_entry(&root, local("2026-08-20", "12:13:14"), "andra texten").unwrap();

        let entries = read_recent(&root, 5);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "andra texten");
        assert_eq!(entries[0].at.to_string(), "2026-08-20 12:13:14");
        assert_eq!(entries[1].text, "första texten");
        assert_eq!(entries[1].at.to_string(), "2026-08-20 09:10:11");
    }

    #[test]
    fn reads_across_days_in_order_and_respects_limit() {
        let root = temp_dir();
        append_entry(&root, local("2026-08-18", "20:00:00"), "old").unwrap();
        append_entry(&root, local("2026-08-19", "08:00:00"), "middle").unwrap();
        append_entry(&root, local("2026-08-20", "07:00:00"), "new").unwrap();

        let entries = read_recent(&root, 2);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["new", "middle"]
        );
    }

    #[test]
    fn reads_conflict_copy_without_changing_either_file() {
        let root = temp_dir();
        let canonical = root.join("2026-10-13.md");
        let conflict = root.join("2026-10-13 2.md");
        fs::write(&canonical, "## 09:00:00\n\nFirst\n\n").unwrap();
        fs::write(&conflict, "## 10:00:00\n\nSecond\n\n").unwrap();
        let entries = read_recent(&root, 2);
        assert_eq!(
            entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["Second", "First"]
        );
        assert_eq!(
            fs::read_to_string(canonical).unwrap(),
            "## 09:00:00\n\nFirst\n\n"
        );
        assert_eq!(
            fs::read_to_string(conflict).unwrap(),
            "## 10:00:00\n\nSecond\n\n"
        );
        assert!(day_file_date("2026-10-13 note").is_none());
        let fix = Fix {
            file: "2026-10-13 2".into(),
            start_ms: local("2026-10-13", "10:00:00").timestamp_millis(),
            original: "Second".into(),
            edited: "Fixed second".into(),
        };
        replace_entry(&root, &fix).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("2026-10-13 2.md")).unwrap(),
            "## 10:00:00\n\nFixed second\n\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("2026-10-13.md")).unwrap(),
            "## 09:00:00\n\nFirst\n\n"
        );
    }

    #[test]
    fn heals_moved_and_merged_day_files() {
        let root = temp_dir();
        let preferred = root.join("preferred");
        let fallback = root.join("fallback");
        std::fs::create_dir_all(&preferred).unwrap();
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(fallback.join("2026-08-18.md"), "moved\n").unwrap();
        std::fs::write(preferred.join("2026-08-19.md"), "preferred").unwrap();
        std::fs::write(fallback.join("2026-08-19.md"), "fallback\n").unwrap();

        assert_eq!(resolve_root(&preferred, &fallback), preferred);
        assert_eq!(
            std::fs::read_to_string(preferred.join("2026-08-18.md")).unwrap(),
            "moved\n"
        );
        assert_eq!(
            std::fs::read_to_string(preferred.join("2026-08-19.md")).unwrap(),
            "preferred\nfallback\n"
        );
        assert!(!fallback.join("2026-08-18.md").exists());
        assert!(!fallback.join("2026-08-19.md").exists());
    }

    #[test]
    fn falls_back_when_preferred_parent_is_unwritable() {
        let root = temp_dir();
        let blocked = root.join("blocked");
        let fallback = root.join("fallback");
        std::fs::create_dir_all(&blocked).unwrap();
        let mut permissions = std::fs::metadata(&blocked).unwrap().permissions();
        permissions.set_mode(0o555);
        std::fs::set_permissions(&blocked, permissions).unwrap();

        let resolved = resolve_root(&blocked.join("history"), &fallback);

        let mut permissions = std::fs::metadata(&blocked).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&blocked, permissions).unwrap();
        assert_eq!(resolved, fallback);
    }

    #[test]
    fn repeated_hour_edits_the_selected_occurrence() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let explicit = "## 02:15:00 +02:00\n\nsame\n\n## 02:15:00 +01:00\n\nsame\n\n";
        let mut cases = vec![explicit.to_owned()];
        // Run under TZ=Europe/Stockholm to exercise the legacy fold.
        if matches!(
            Local.from_local_datetime(&date.and_hms_opt(2, 15, 0).unwrap()),
            chrono::LocalResult::Ambiguous(_, _)
        ) {
            cases.push(explicit.replace(" +02:00", "").replace(" +01:00", ""));
        }
        for text in cases {
            let entries = day_entries(date, &text);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[1].start_ms - entries[0].start_ms, 3_600_000);
            let fix = Fix {
                file: date.to_string(),
                start_ms: entries[1].start_ms,
                original: "same".into(),
                edited: "second".into(),
            };
            let after = replaced(&text, &fix).unwrap();
            assert_eq!(
                day_entries(date, &after)
                    .iter()
                    .map(|e| e.text.as_str())
                    .collect::<Vec<_>>(),
                ["same", "second"]
            );
        }
    }

    #[test]
    fn a_fix_replaces_one_entry_and_keeps_every_other_byte() {
        let day = "## 09:00:00\n\nuse potato mode\n\n## 09:00:00\n\nsame second\n\n## 10:15:00\n\nlast one\n\n";
        let fix = |time: &str, original: &str, edited: &str| Fix {
            file: "2026-10-15".into(),
            start_ms: local("2026-10-15", time).timestamp_millis(),
            original: original.into(),
            edited: edited.into(),
        };
        assert_eq!(
            replaced(day, &fix("09:00:00", "same second", "Same second, fixed")).as_deref(),
            Some(
                day.replace("same second\n", "Same second, fixed\n")
                    .as_str()
            )
        );
        assert_eq!(
            replaced(day, &fix("10:15:00", " last one ", "Last one.")).as_deref(),
            Some(day.replace("last one\n", "Last one.\n").as_str())
        );
        assert_eq!(
            replaced(day, &fix("09:00:00", "not there", "x")),
            None,
            "an entry that changed on disk is not overwritten"
        );
    }

    #[test]
    fn transcript_body_round_trips_verbatim() {
        let root = temp_dir();
        let text = "Det här är ord för ord — på svenska.";
        append_entry(&root, local("2026-08-20", "12:00:00"), text).unwrap();
        assert_eq!(read_recent(&root, 1)[0].text, text);
    }
}
