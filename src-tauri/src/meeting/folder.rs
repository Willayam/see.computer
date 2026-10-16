use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone};

pub const MANIFEST: &str = "meeting.json";
pub const MARKDOWN: &str = "meeting.md";
pub const YOU: &str = "you.wav";
pub const THEM: &str = "them.wav";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Recording,
    Transcribing,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub app: Option<String>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Manifest {
    pub fn duration_ms(&self) -> Option<u64> {
        self.ended_ms
            .map(|end| end.saturating_sub(self.started_ms) as u64)
    }
}

pub fn title(folder: &Path, manifest: &Manifest) -> String {
    if let Some(app) = &manifest.app {
        return format!("{app} meeting");
    }
    let text = fs::read_to_string(folder.join(MARKDOWN)).unwrap_or_default();
    let Some(line) = text.lines().find_map(|line| {
        line.split_once(" Them: ")
            .map(|(_, text)| text.trim())
            .filter(|text| !text.is_empty())
    }) else {
        return "Meeting".to_owned();
    };
    let words = line
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    let shortened = words.chars().take(40).collect::<String>();
    if shortened.len() < line.len() {
        format!("{}…", shortened.trim_end())
    } else {
        shortened
    }
}

pub fn create(root: &Path, app: Option<&str>, started_ms: i64) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    let started = Local
        .timestamp_millis_opt(started_ms)
        .single()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid start time"))?;
    let name = format!("meeting-{}", started.format("%Y-%m-%d-%H-%M-%S"));
    for index in 1.. {
        let folder = root.join(if index == 1 {
            name.clone()
        } else {
            format!("{name}-{index}")
        });
        match fs::create_dir(&folder) {
            Ok(()) => {
                let manifest = Manifest {
                    app: app.map(str::to_owned),
                    started_ms,
                    ended_ms: None,
                    state: State::Recording,
                    error: None,
                };
                write(&folder, &manifest)?;
                return Ok(folder);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

pub fn read(folder: &Path) -> Option<Manifest> {
    serde_json::from_slice(&fs::read(folder.join(MANIFEST)).ok()?).ok()
}

pub fn write(folder: &Path, manifest: &Manifest) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    let temp = folder.join(format!(".{MANIFEST}.{}.tmp", std::process::id()));
    fs::write(&temp, bytes)?;
    if let Err(error) = fs::rename(&temp, folder.join(MANIFEST)) {
        let _ = fs::remove_file(temp);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn titles_use_app_or_first_other_voice() {
        let root = std::env::temp_dir().join(format!(
            "see-meeting-title-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut manifest = Manifest {
            app: Some("Zoom".to_owned()),
            started_ms: 0,
            ended_ms: None,
            state: State::Done,
            error: None,
        };
        fs::write(
            root.join(MARKDOWN),
            "[00:01] You: Hello.\n[00:02] Them: This is the first reply from the other person.\n",
        )
        .unwrap();
        assert_eq!(title(&root, &manifest), "Zoom meeting");
        manifest.app = None;
        assert_eq!(title(&root, &manifest), "This is the first reply from…");
        fs::write(root.join(MARKDOWN), "[00:01] You: Hello.\n").unwrap();
        assert_eq!(title(&root, &manifest), "Meeting");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creates_reads_and_updates_unique_folders() {
        let root = std::env::temp_dir().join(format!(
            "see-meeting-folder-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = create(&root, Some("Zoom"), 1_781_000_000_000).unwrap();
        assert_eq!(read(&first).unwrap().state, State::Recording);
        let second = create(&root, None, 1_781_000_000_000).unwrap();
        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            format!("{}-2", first.file_name().unwrap().to_string_lossy())
        );
        let mut manifest = read(&first).unwrap();
        manifest.state = State::Done;
        manifest.ended_ms = Some(manifest.started_ms + 60_000);
        write(&first, &manifest).unwrap();
        assert_eq!(read(&first), Some(manifest));
        fs::remove_dir_all(root).unwrap();
    }
}
