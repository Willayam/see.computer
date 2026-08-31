//! The five most recent dictations and takes, merged by time.

use chrono::{DateTime, Local, NaiveDateTime};
use std::path::{Path, PathBuf};

use crate::history::{Entry, History};

/// Both kinds of recent name themselves, so the glyph column reads as a pair
/// rather than as one row being the exception: spoken words, or a screen.
const DICTATION: &str = "waveform";
const CLIP: &str = "video.fill";

#[derive(Clone)]
pub(crate) enum Payload {
    Transcript(String),
    /// A recording that was packaged into a clip folder: the row reads as the
    /// narration and copies the paragraph the recording pasted.
    Clip(crate::clip::Summary),
    /// A recording with no clip folder beside it — packaging failed, or the
    /// movie predates the folder. The plain `file://` link is all there is.
    Recording(PathBuf),
}

impl Payload {
    pub fn copy_text(self) -> String {
        match self {
            Payload::Transcript(text) => text,
            Payload::Clip(summary) => summary.paste(),
            Payload::Recording(path) => file_link(&path),
        }
    }
}

fn file_link(path: &Path) -> String {
    let plain = path.to_string_lossy().into_owned();
    if path.is_absolute() {
        url::Url::from_file_path(path)
            .map(|url| url.to_string())
            .unwrap_or(plain)
    } else {
        plain
    }
}

pub struct Recent {
    at: NaiveDateTime,
    pub payload: Payload,
}

impl Recent {
    /// What a recent reads as: the words when there are any, and the glyph that
    /// says whether they were spoken into the cursor or over a screen recording.
    pub fn row(&self) -> (String, &'static str) {
        match &self.payload {
            Payload::Transcript(text) => (transcript_label(text), DICTATION),
            Payload::Clip(summary) => (
                summary
                    .text
                    .as_deref()
                    .map_or_else(|| recording_label(self.at), transcript_label),
                CLIP,
            ),
            Payload::Recording(_) => (recording_label(self.at), CLIP),
        }
    }
}

pub fn recent(history: &History, limit: usize) -> Vec<Recent> {
    merge_recents(
        history.recent(limit),
        recent_recordings(&crate::paths::documents(), limit),
        limit,
    )
}

fn recent_recordings(dir: &Path, limit: usize) -> Vec<(DateTime<Local>, PathBuf)> {
    let mut recordings = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            entry
                .metadata()
                .ok()
                .filter(|metadata| {
                    metadata.is_file()
                        && path.extension().is_some_and(|extension| extension == "mov")
                        || metadata.is_dir()
                            && path.join("clips").is_dir()
                            && (path.join("take.md").is_file() || path.join("clip.md").is_file())
                })
                .and_then(|metadata| metadata.modified().ok())
                .map(|modified| (DateTime::<Local>::from(modified), path))
        })
        .collect::<Vec<_>>();
    recordings.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    recordings.truncate(limit);
    recordings
}

fn merge_recents(
    transcripts: Vec<Entry>,
    recordings: Vec<(DateTime<Local>, PathBuf)>,
    limit: usize,
) -> Vec<Recent> {
    let mut recents = transcripts
        .into_iter()
        .map(|entry| Recent {
            at: entry.at,
            payload: Payload::Transcript(entry.text),
        })
        .chain(recordings.into_iter().map(|(at, path)| Recent {
            at: at.naive_local(),
            payload: recording_payload(path),
        }))
        .collect::<Vec<_>>();
    recents.sort_by_key(|recent| std::cmp::Reverse(recent.at));
    recents.truncate(limit);
    recents
}

/// The take folder is what the recording pasted, so the row follows it when
/// it is there and falls back to the movie when it is not.
fn recording_payload(path: PathBuf) -> Payload {
    match crate::clip::summary(&path) {
        Some(summary) => Payload::Clip(summary),
        None => Payload::Recording(path),
    }
}

fn transcript_label(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

fn recording_label(at: NaiveDateTime) -> String {
    if at.date() == Local::now().date_naive() {
        format!("Recording · {}", at.format("%H:%M"))
    } else {
        format!("Recording · {}", at.format("%b %-d, %H:%M"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::{Duration, SystemTime};

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("see-tray-test-{unique}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn local(date: &str, time: &str) -> DateTime<Local> {
        let naive =
            NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S").unwrap();
        Local.from_local_datetime(&naive).single().unwrap()
    }

    #[test]
    fn recent_recordings_are_newest_first_and_limited() {
        let dir = temp_dir();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        for (name, offset) in [("old.mov", 1), ("new.mov", 3), ("middle.mov", 2)] {
            let file = std::fs::File::create(dir.join(name)).unwrap();
            file.set_times(
                std::fs::FileTimes::new().set_modified(base + Duration::from_secs(offset)),
            )
            .unwrap();
        }
        std::fs::write(dir.join("ignored.txt"), "not a recording").unwrap();

        let recordings = recent_recordings(&dir, 2);
        assert_eq!(recordings.len(), 2);
        assert_eq!(recordings[0].1.file_name().unwrap(), "new.mov");
        assert_eq!(recordings[1].1.file_name().unwrap(), "middle.mov");
    }

    #[test]
    fn recent_recordings_include_nested_take_directories() {
        let dir = temp_dir();
        let take = dir.join("2026-08-28-10-12-09");
        std::fs::create_dir_all(take.join("clips/001")).unwrap();
        std::fs::write(take.join("clips/001/clip.mov"), b"movie").unwrap();
        std::fs::write(take.join("take.md"), "# Screen take").unwrap();

        let recordings = recent_recordings(&dir, 10);

        assert!(recordings.iter().any(|(_, path)| path == &take));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn transcript_labels_collapse_whitespace_and_cap_on_chars() {
        assert_eq!(transcript_label("  hej\n  på\t dig  "), "hej på dig");
        let long = "å".repeat(121);
        let label = transcript_label(&long);
        assert_eq!(label.chars().count(), 120);
        assert!(label.chars().all(|character| character == 'å'));
    }

    #[test]
    fn a_packaged_recording_carries_its_take_paste() {
        let dir = temp_dir();
        let mov = dir.join("2026-08-26_14-32-01.mov");
        std::fs::write(&mov, b"not a movie").unwrap();
        let clip = mov.with_extension("");
        std::fs::create_dir_all(&clip).unwrap();
        std::fs::write(clip.join("clip.md"), "# Screen recording with narration").unwrap();
        std::fs::write(
            clip.join("transcript.json"),
            r#"{"durationMs": 25000, "fullText": "Look at the misaligned button."}"#,
        )
        .unwrap();

        let merged = merge_recents(Vec::new(), recent_recordings(&dir, 1), 1);
        let Payload::Clip(summary) = &merged[0].payload else {
            panic!("a movie with a clip folder is a clip");
        };
        assert_eq!(
            summary.text.as_deref(),
            Some("Look at the misaligned button.")
        );
        assert_eq!(
            summary.paste(),
            format!(
                "\"Look at the misaligned button.\"\n\n1 clip (0:25): {}",
                clip.join("clip.md").display()
            )
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unpackaged_recording_stays_a_plain_movie() {
        let dir = temp_dir();
        std::fs::write(dir.join("lonely.mov"), b"not a movie").unwrap();

        let merged = merge_recents(Vec::new(), recent_recordings(&dir, 1), 1);
        assert!(
            matches!(&merged[0].payload, Payload::Recording(path) if path.ends_with("lonely.mov"))
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_kind_of_recent_names_itself() {
        let at = local("2026-08-20", "14:32:00").naive_local();
        let dictation = Recent {
            at,
            payload: Payload::Transcript("Hej på dig".to_owned()),
        };
        assert_eq!(dictation.row(), ("Hej på dig".to_owned(), DICTATION));

        let clip = Recent {
            at,
            payload: Payload::Clip(crate::clip::Summary {
                markdown: PathBuf::from("/tmp/demo/take.md"),
                text: Some("Look at the button.".to_owned()),
                screenshot_count: 0,
                clip_count: 1,
                clip_duration_ms: 25_000,
            }),
        };
        assert_eq!(clip.row(), ("Look at the button.".to_owned(), CLIP));

        let silent = Recent {
            at,
            payload: Payload::Clip(crate::clip::Summary {
                markdown: PathBuf::from("/tmp/demo/take.md"),
                text: None,
                screenshot_count: 0,
                clip_count: 1,
                clip_duration_ms: 25_000,
            }),
        };
        assert_eq!(silent.row(), ("Recording · Aug 20, 14:32".to_owned(), CLIP));
    }

    #[test]
    fn merges_transcripts_and_recordings_by_timestamp() {
        let transcripts = vec![
            Entry {
                at: local("2026-08-20", "12:00:00").naive_local(),
                text: "newest".to_owned(),
            },
            Entry {
                at: local("2026-08-20", "10:00:00").naive_local(),
                text: "oldest".to_owned(),
            },
        ];
        let recordings = vec![(local("2026-08-20", "11:00:00"), PathBuf::from("middle.mov"))];

        let merged = merge_recents(transcripts, recordings, 3);
        assert!(matches!(
            &merged[0].payload,
            Payload::Transcript(text) if text == "newest"
        ));
        assert!(
            matches!(&merged[1].payload, Payload::Recording(path) if path == Path::new("middle.mov"))
        );
        assert!(matches!(
            &merged[2].payload,
            Payload::Transcript(text) if text == "oldest"
        ));
    }
}
