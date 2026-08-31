use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct History {
    root: Arc<Mutex<PathBuf>>,
    enabled: Arc<AtomicBool>,
    tx: Sender<String>,
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
        let (tx, rx) = std::sync::mpsc::channel::<String>();
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
            while let Ok(text) = rx.recv() {
                let at = Local::now();
                let live = locked_root(&writer_root);
                if let Err(error) = append_entry(&live, at, &text) {
                    if can_fallback && error.raw_os_error() == Some(libc::EPERM) {
                        let _ = std::fs::create_dir_all(&fallback);
                        set_root(&writer_root, fallback.clone());
                        let _ = append_entry(&fallback, at, &text);
                    }
                }
            }
        });
        History { root, enabled, tx }
    }

    pub fn record(&self, text: &str) {
        if self.enabled() {
            let _ = self.tx.send(text.to_owned());
        }
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
    let mut days = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "md") {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok())
                    .map(|date| (date, path))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    days.sort_by_key(|(date, _)| std::cmp::Reverse(*date));

    let mut entries = Vec::new();
    for (date, path) in days.into_iter().take(14) {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        entries.extend(parse_day(date, &content));
        if entries.len() >= limit {
            entries.truncate(limit);
            break;
        }
    }
    entries
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
    fn transcript_body_round_trips_verbatim() {
        let root = temp_dir();
        let text = "Det här är ord för ord — på svenska.";
        append_entry(&root, local("2026-08-20", "12:00:00"), text).unwrap();
        assert_eq!(read_recent(&root, 1)[0].text, text);
    }
}
