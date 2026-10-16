use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime};
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
}

/// The history thread is the only writer of day files, so a fix made in the
/// Library cannot race the append of the next dictation.
enum Request {
    Append(String),
    Replace(Fix, Sender<Result<(), String>>),
}

/// One dictation's text as the Library showed it, and what the user changed
/// it to.
pub struct Fix {
    pub file: String,
    pub at: NaiveDateTime,
    pub original: String,
    pub edited: String,
}

pub struct Entry {
    pub at: NaiveDateTime,
    pub text: String,
}

impl History {
    pub fn start(enabled: bool) -> History {
        let preferred = crate::paths::documents();
        let fallback = crate::paths::history_fallback();
        let seam = std::env::var_os("SEE_COMPUTER_HISTORY_DIR").map(PathBuf::from);
        let initial = seam.clone().unwrap_or_else(|| preferred.clone());
        let root = Arc::new(Mutex::new(initial));
        let enabled = Arc::new(AtomicBool::new(enabled));
        let (tx, rx) = mpsc::channel::<Request>();
        let writer_root = root.clone();
        crate::qos::spawn("see-history", crate::qos::Class::Upkeep, move || {
            let (live, can_fallback) = match seam {
                Some(path) => {
                    let _ = std::fs::create_dir_all(&path).and_then(|()| touch_today(&path));
                    (path, false)
                }
                None => (resolve_root(&preferred, &fallback), true),
            };
            set_root(&writer_root, live);
            while let Ok(write) = rx.recv() {
                let text = match write {
                    Request::Append(text) => text,
                    Request::Replace(fix, reply) => {
                        let result = replace_entry(&locked_root(&writer_root), &fix);
                        if result.is_ok() {
                            crate::library::changed();
                        }
                        let _ = reply.send(result);
                        continue;
                    }
                };
                let at = Local::now();
                let live = locked_root(&writer_root);
                if let Err(error) = append_entry(&live, at, &text) {
                    if can_fallback && error.raw_os_error() == Some(libc::EPERM) {
                        let _ = std::fs::create_dir_all(&fallback);
                        set_root(&writer_root, fallback.clone());
                        if append_entry(&fallback, at, &text).is_ok() {
                            crate::library::changed();
                        }
                    }
                } else {
                    crate::library::changed();
                }
            }
        });
        History { root, enabled, tx }
    }

    pub fn record(&self, text: &str) {
        if self.enabled() {
            let _ = self.tx.send(Request::Append(text.to_owned()));
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

fn append_entry(root: &Path, at: DateTime<Local>, text: &str) -> io::Result<()> {
    let path = root.join(format!("{}.md", at.format("%Y-%m-%d")));
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    write!(file, "## {}\n\n{text}\n\n", at.format("%H:%M:%S"))
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
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.at));
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

fn day_files(root: &Path) -> Vec<(NaiveDate, PathBuf)> {
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
    if day_file_date(&fix.file) != Some(fix.at.date()) {
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
    let heading = format!("## {}\n\n", fix.at.format("%H:%M:%S"));
    let mut from = 0;
    while let Some(found) = content[from..].find(&heading) {
        let start = from + found;
        from = start + heading.len();
        if start > 0 && content.as_bytes()[start - 1] != b'\n' {
            continue;
        }
        let body_start = start + heading.len();
        let body_end = content[body_start..]
            .match_indices("\n## ")
            .map(|(index, _)| body_start + index + 1)
            .find(|&next| {
                let rest = &content[next + 3..];
                rest.get(..8)
                    .is_some_and(|time| NaiveTime::parse_from_str(time, "%H:%M:%S").is_ok())
            })
            .unwrap_or(content.len());
        let body = &content[body_start..body_end];
        if body.trim() != fix.original.trim() {
            continue;
        }
        let tail = &body[body.trim_end().len()..];
        return Some(format!(
            "{}{}{}{}",
            &content[..body_start],
            fix.edited.trim(),
            tail,
            &content[body_end..]
        ));
    }
    None
}

fn parse_day(date: NaiveDate, content: &str) -> Vec<Entry> {
    let mut headings = Vec::new();
    for (start, _) in content.match_indices("## ") {
        if start > 0 && content.as_bytes().get(start - 1) != Some(&b'\n') {
            continue;
        }
        let heading = &content[start + 3..];
        let Some(line_end) = heading.find('\n') else {
            continue;
        };
        let Ok(time) = NaiveTime::parse_from_str(&heading[..line_end], "%H:%M:%S") else {
            continue;
        };
        let body_start = start + 3 + line_end + 1;
        if content.as_bytes().get(body_start) != Some(&b'\n') {
            continue;
        }
        headings.push((start, body_start + 1, time));
    }

    let mut entries = headings
        .iter()
        .enumerate()
        .map(|(index, (_, body_start, time))| {
            let body_end = headings
                .get(index + 1)
                .map_or(content.len(), |(start, _, _)| *start);
            let body = &content[*body_start..body_end];
            let text = body.strip_suffix("\n\n").unwrap_or(body).to_owned();
            Entry {
                at: date.and_time(*time),
                text,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.at));
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
            at: NaiveDate::from_ymd_opt(2026, 10, 13)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
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
    fn a_fix_replaces_one_entry_and_keeps_every_other_byte() {
        let day = "## 09:00:00\n\nuse potato mode\n\n## 09:00:00\n\nsame second\n\n## 10:15:00\n\nlast one\n\n";
        let fix = |time: &str, original: &str, edited: &str| Fix {
            file: "2026-10-15".into(),
            at: NaiveDate::from_ymd_opt(2026, 10, 15)
                .unwrap()
                .and_time(NaiveTime::parse_from_str(time, "%H:%M:%S").unwrap()),
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
