//! Package a finished recording into an agent-readable clip folder.
//!
//! Next to `<stem>.mov` this writes:
//!
//! ```text
//! <stem>/clip.md          the file whose path gets pasted
//! <stem>/transcript.json  the same segments, machine-readable
//! <stem>/frames/<ms>.jpg  one screen frame per interesting moment
//! ```
//!
//! The markdown interleaves frames with the timestamped transcript so an agent
//! can read what was said and open the screen exactly where it was said,
//! without ever decoding the video.

use std::fmt::Write as _;
use std::os::raw::{c_char, c_int, c_longlong};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::engine::{Segment, Transcription};
use crate::mic::Audio16k;

/// Matches the cap and spacing Clips uses for its recommended frames.
pub const MAX_FRAMES: usize = 16;
const MIN_FRAME_GAP_MS: u64 = 3_000;
const FRAME_MAX_WIDTH: i32 = 1_280;

extern "C" {
    fn sc_clip_duration_seconds(path: *const c_char) -> f64;
    fn sc_frame_jpeg(
        path: *const c_char,
        at_ms: c_longlong,
        max_width: c_int,
        out_path: *const c_char,
    ) -> c_int;
}

fn c_path(path: &Path) -> Option<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).ok()
}

/// The audio track as the engine wants it, via the system `afconvert`.
/// `None` when the movie has no readable audio (mic permission was off).
pub fn extract_audio(mov: &Path) -> Option<Audio16k> {
    let wav = std::env::temp_dir().join(format!(
        "see-computer-clip-{}-{:x}.wav",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|at| at.as_nanos())
            .unwrap_or(0)
    ));
    let status = std::process::Command::new("/usr/bin/afconvert")
        .arg(mov)
        .arg(&wav)
        .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let audio = match status {
        Ok(status) if status.success() => Audio16k::from_wav(&wav).ok(),
        _ => None,
    };
    let _ = std::fs::remove_file(&wav);
    audio
}

/// What a packaged clip folder says about itself, read back from disk. The
/// panel draws its recent rows from this, so the JSON keys stay in one module.
#[derive(Clone)]
pub struct Summary {
    pub markdown: PathBuf,
    pub duration_ms: u64,
    /// The narration, when there was any.
    pub text: Option<String>,
}

impl Summary {
    /// The same paragraph the recording pasted at the cursor.
    pub fn paste(&self) -> String {
        paste_line(self.duration_ms, self.text.as_deref(), &self.markdown)
    }
}

/// Reads the clip folder written next to `<stem>.mov`. `None` when packaging
/// never finished, or the movie predates the clip folder.
pub fn summary(mov: &Path) -> Option<Summary> {
    let dir = mov.with_extension("");
    let markdown = dir.join("clip.md");
    if dir == mov || !markdown.is_file() {
        return None;
    }
    let transcript = std::fs::read_to_string(dir.join("transcript.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or(serde_json::Value::Null);
    Some(Summary {
        markdown,
        duration_ms: transcript
            .get("durationMs")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        text: transcript
            .get("fullText")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
    })
}

pub struct Packaged {
    pub markdown: PathBuf,
    /// What lands at the cursor: the spoken words first, so the paste reads as
    /// a message anywhere, then the `clip.md` path for agents that can follow it.
    pub paste: String,
}

pub struct Shot {
    pub at_ms: u64,
    pub path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("recording has no file stem")]
    NoStem,
    #[error("could not write clip folder: {0}")]
    Io(#[from] std::io::Error),
}

pub fn package(mov: &Path, transcription: &Transcription) -> Result<Packaged, PackageError> {
    let dir = mov.with_extension("");
    if dir == mov || mov.file_stem().is_none() {
        return Err(PackageError::NoStem);
    }
    let frames_dir = dir.join("frames");
    let _ = std::fs::remove_dir_all(&frames_dir);
    std::fs::create_dir_all(&frames_dir)?;

    let duration_ms = duration_ms(mov, &transcription.segments);
    let mut frames = Vec::new();
    for at in frame_times(duration_ms, &transcription.segments) {
        let file = frames_dir.join(format!("{at:07}.jpg"));
        if extract_frame(mov, at, &file) {
            frames.push(at);
        }
    }

    let mov_name = mov
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::fs::write(
        dir.join("transcript.json"),
        transcript_json(&mov_name, duration_ms, transcription, &frames),
    )?;
    let markdown = dir.join("clip.md");
    std::fs::write(
        &markdown,
        markdown_index(&mov_name, duration_ms, &transcription.segments, &frames),
    )?;
    let paste = paste_line(
        duration_ms,
        transcription.text.as_ref().map(|text| text.as_str()),
        &markdown,
    );
    Ok(Packaged { markdown, paste })
}

/// One plain paragraph: quoted narration, then the folder path. Humans and
/// web chatboxes get the words even where a local path is dead; file-capable
/// agents take the path to the frames and video.
pub fn paste_line(duration_ms: u64, full_text: Option<&str>, markdown: &Path) -> String {
    let length = if duration_ms > 0 {
        format!(" ({})", timestamp(duration_ms))
    } else {
        String::new()
    };
    match full_text {
        Some(text) if !text.is_empty() => format!(
            "Screen recording{length}: \"{text}\" \u{2014} screen frames and video: {}",
            markdown.display()
        ),
        _ => format!(
            "Screen recording{length}, no narration \u{2014} screen frames and video: {}",
            markdown.display()
        ),
    }
}

pub fn package_shots(
    dir: &Path,
    duration_ms: u64,
    transcription: &Transcription,
    shots: &[Shot],
) -> Result<Packaged, PackageError> {
    std::fs::create_dir_all(dir.join("shots"))?;
    let mut shots = shots.iter().collect::<Vec<_>>();
    shots.sort_by_key(|shot| shot.at_ms);
    let duration_ms = duration_ms.max(
        transcription
            .segments
            .last()
            .map(|segment| segment.end_ms)
            .unwrap_or(0),
    );
    std::fs::write(
        dir.join("transcript.json"),
        shots_transcript_json(dir, duration_ms, transcription, &shots),
    )?;
    let markdown = dir.join("session.md");
    std::fs::write(
        &markdown,
        shots_markdown_index(dir, duration_ms, &transcription.segments, &shots),
    )?;
    let paste = shots_paste_line(
        duration_ms,
        transcription.text.as_ref().map(|text| text.as_str()),
        &markdown,
    );
    Ok(Packaged { markdown, paste })
}

/// The folder carries the capture structure so the cursor only needs the
/// narration and one path, the shape that made agents inspect selectively.
pub fn shots_paste_line(duration_ms: u64, full_text: Option<&str>, markdown: &Path) -> String {
    let length = if duration_ms > 0 {
        format!(" ({})", timestamp(duration_ms))
    } else {
        String::new()
    };
    match full_text {
        Some(text) if !text.is_empty() => format!(
            "Screen session{length}: \"{text}\" \u{2014} screenshots: {}",
            markdown.display()
        ),
        _ => format!(
            "Screen session{length}, no narration \u{2014} screenshots: {}",
            markdown.display()
        ),
    }
}

fn shots_markdown_index(
    dir: &Path,
    duration_ms: u64,
    segments: &[Segment],
    shots: &[&Shot],
) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Screen session with screenshots\n");
    let length = if duration_ms > 0 {
        format!("{} long, ", timestamp(duration_ms))
    } else {
        String::new()
    };
    let _ = writeln!(
        md,
        "{length}captured with see.computer. The transcript below is timestamped, and each \
         screenshot sits with the sentence being spoken when it was taken. The timings and \
         paths are also in [`transcript.json`](transcript.json).\n",
    );
    let _ = writeln!(md, "## Transcript\n");
    if segments.is_empty() {
        let _ = writeln!(md, "No speech was detected.\n");
        for shot in shots {
            write_shot(&mut md, dir, shot);
        }
        return md;
    }

    let mut remaining = shots.iter().peekable();
    for segment in segments {
        while let Some(shot) = remaining.peek() {
            if shot.at_ms < segment.start_ms {
                write_shot(&mut md, dir, shot);
                remaining.next();
            } else {
                break;
            }
        }
        let _ = writeln!(
            md,
            "**{}\u{2013}{}** {}\n",
            timestamp(segment.start_ms),
            timestamp(segment.end_ms),
            segment.text
        );
        while let Some(shot) = remaining.peek() {
            if shot.at_ms < segment.end_ms {
                write_shot(&mut md, dir, shot);
                remaining.next();
            } else {
                break;
            }
        }
    }
    for shot in remaining {
        write_shot(&mut md, dir, shot);
    }
    md
}

fn write_shot(md: &mut String, dir: &Path, shot: &Shot) {
    let file = shot_file(dir, shot);
    let _ = writeln!(md, "![{}]({file})\n", timestamp(shot.at_ms));
}

fn shot_file(dir: &Path, shot: &Shot) -> String {
    shot.path
        .strip_prefix(dir)
        .unwrap_or(&shot.path)
        .to_string_lossy()
        .into_owned()
}

fn shots_transcript_json(
    dir: &Path,
    duration_ms: u64,
    transcription: &Transcription,
    shots: &[&Shot],
) -> String {
    let segments: Vec<serde_json::Value> = transcription
        .segments
        .iter()
        .map(|segment| {
            serde_json::json!({
                "startMs": segment.start_ms,
                "endMs": segment.end_ms,
                "range": format!("{}-{}", timestamp(segment.start_ms), timestamp(segment.end_ms)),
                "text": segment.text,
            })
        })
        .collect();
    let captures: Vec<serde_json::Value> = shots
        .iter()
        .map(|shot| {
            serde_json::json!({
                "type": "shot",
                "atMs": shot.at_ms,
                "timestamp": timestamp(shot.at_ms),
                "file": shot_file(dir, shot),
            })
        })
        .collect();
    let value = serde_json::json!({
        "type": "see-computer.session",
        "version": 1,
        "durationMs": duration_ms,
        "fullText": transcription.text.as_ref().map(|text| text.as_str()).unwrap_or(""),
        "segments": segments,
        "captures": captures,
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_owned())
}

fn extract_frame(mov: &Path, at_ms: u64, out: &Path) -> bool {
    let (Some(mov), Some(out)) = (c_path(mov), c_path(out)) else {
        return false;
    };
    unsafe {
        sc_frame_jpeg(
            mov.as_ptr(),
            at_ms as c_longlong,
            FRAME_MAX_WIDTH,
            out.as_ptr(),
        ) == 0
    }
}

fn duration_ms(mov: &Path, segments: &[Segment]) -> u64 {
    let native = c_path(mov)
        .map(|path| unsafe { sc_clip_duration_seconds(path.as_ptr()) })
        .unwrap_or(-1.0);
    if native > 0.0 {
        return (native * 1000.0).round() as u64;
    }
    segments.last().map(|segment| segment.end_ms).unwrap_or(0)
}

/// Opening frame, then one per sentence start, then quarter marks when there is
/// no speech to follow. Frames closer than [`MIN_FRAME_GAP_MS`] show the same
/// screen, so near-duplicates are dropped, like Clips' recommended frames.
pub fn frame_times(duration_ms: u64, segments: &[Segment]) -> Vec<u64> {
    let mut times: Vec<u64> = Vec::new();
    let mut push = |at: u64| {
        let at = if duration_ms > 0 {
            at.min(duration_ms.saturating_sub(100))
        } else {
            at
        };
        if times.len() < MAX_FRAMES && times.iter().all(|t| t.abs_diff(at) >= MIN_FRAME_GAP_MS) {
            times.push(at);
        }
    };
    push(0);
    for segment in segments {
        push(segment.start_ms);
    }
    if segments.is_empty() {
        for percent in [25, 50, 75] {
            push(duration_ms * percent / 100);
        }
    }
    times.sort_unstable();
    times
}

pub fn timestamp(ms: u64) -> String {
    let total = ms / 1000;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn markdown_index(
    mov_name: &str,
    duration_ms: u64,
    segments: &[Segment],
    frames: &[u64],
) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Screen recording with narration\n");
    let length = if duration_ms > 0 {
        format!("{} long, ", timestamp(duration_ms))
    } else {
        String::new()
    };
    let _ = writeln!(
        md,
        "{length}recorded with see.computer. This folder is the agent-readable clip: the \
         transcript below is timestamped, and each image is the screen at that moment, so you \
         can read AND see it. Open the frames whose timestamps matter for your task; every \
         extracted frame is in `frames/`, the segments are in [`transcript.json`](transcript.json), \
         and the full video is [`{mov_name}`](../{mov_name}).\n",
    );
    let _ = writeln!(md, "## Transcript\n");
    if segments.is_empty() {
        let _ = writeln!(
            md,
            "No speech was detected; the frames below sample the screen instead.\n"
        );
        for at in frames {
            let _ = writeln!(md, "![{}](frames/{at:07}.jpg)\n", timestamp(*at));
        }
        return md;
    }
    let mut remaining = frames.iter().peekable();
    for segment in segments {
        while let Some(at) = remaining.peek() {
            if **at <= segment.start_ms {
                let _ = writeln!(md, "![{}](frames/{at:07}.jpg)\n", timestamp(**at));
                remaining.next();
            } else {
                break;
            }
        }
        let _ = writeln!(
            md,
            "**{}\u{2013}{}** {}\n",
            timestamp(segment.start_ms),
            timestamp(segment.end_ms),
            segment.text
        );
    }
    for at in remaining {
        let _ = writeln!(md, "![{}](frames/{at:07}.jpg)\n", timestamp(*at));
    }
    md
}

fn transcript_json(
    mov_name: &str,
    duration_ms: u64,
    transcription: &Transcription,
    frames: &[u64],
) -> String {
    let segments: Vec<serde_json::Value> = transcription
        .segments
        .iter()
        .map(|segment| {
            serde_json::json!({
                "startMs": segment.start_ms,
                "endMs": segment.end_ms,
                "range": format!("{}-{}", timestamp(segment.start_ms), timestamp(segment.end_ms)),
                "text": segment.text,
            })
        })
        .collect();
    let frames: Vec<serde_json::Value> = frames
        .iter()
        .map(|at| {
            serde_json::json!({
                "atMs": at,
                "timestamp": timestamp(*at),
                "file": format!("frames/{at:07}.jpg"),
            })
        })
        .collect();
    let value = serde_json::json!({
        "type": "see-computer.clip",
        "version": 1,
        "video": format!("../{mov_name}"),
        "durationMs": duration_ms,
        "fullText": transcription.text.as_ref().map(|text| text.as_str()).unwrap_or(""),
        "segments": segments,
        "frames": frames,
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paste::Text;

    fn segment(start_ms: u64, end_ms: u64, text: &str) -> Segment {
        Segment {
            start_ms,
            end_ms,
            text: text.to_owned(),
        }
    }

    #[test]
    fn frame_times_follow_sentences_with_a_minimum_gap() {
        let segments = [
            segment(400, 3_900, "a"),
            segment(4_000, 7_900, "b"),
            segment(5_000, 7_000, "c"),
            segment(8_000, 9_900, "d"),
        ];
        assert_eq!(frame_times(10_220, &segments), vec![0, 4_000, 8_000]);
    }

    #[test]
    fn frame_times_sample_quarters_without_speech() {
        assert_eq!(frame_times(40_000, &[]), vec![0, 10_000, 20_000, 30_000]);
        assert_eq!(frame_times(0, &[]), vec![0]);
    }

    #[test]
    fn frame_times_clamp_to_the_clip_and_cap_the_count() {
        let segments: Vec<Segment> = (0..40)
            .map(|index| segment(index * 4_000, index * 4_000 + 1_000, "x"))
            .collect();
        let times = frame_times(30_000, &segments);
        assert!(times.len() <= MAX_FRAMES);
        assert!(times.iter().all(|at| *at <= 29_900));
    }

    #[test]
    fn markdown_pairs_frames_with_the_sentences_they_belong_to() {
        let segments = [
            segment(0, 4_000, "First thing."),
            segment(4_200, 9_000, "Second thing."),
        ];
        let md = markdown_index("demo.mov", 10_000, &segments, &[0, 4_200]);
        let first = md.find("First thing.").unwrap();
        let second = md.find("Second thing.").unwrap();
        let frame_two = md.find("frames/0004200.jpg").unwrap();
        assert!(first < frame_two && frame_two < second);
        assert!(md.contains("**0:00\u{2013}0:04** First thing."));
        assert!(md.contains("[`demo.mov`](../demo.mov)"));
    }

    #[test]
    fn package_writes_the_folder_even_when_the_movie_is_unreadable() {
        let dir =
            std::env::temp_dir().join(format!("see-computer-clip-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mov = dir.join("fake.mov");
        std::fs::write(&mov, b"not a movie").unwrap();
        let transcription = Transcription {
            text: Text::parse("Hello there."),
            segments: vec![segment(0, 1_500, "Hello there.")],
        };
        let packaged = package(&mov, &transcription).unwrap();
        assert_eq!(packaged.markdown, dir.join("fake").join("clip.md"));
        let md = std::fs::read_to_string(&packaged.markdown).unwrap();
        assert!(md.contains("Hello there."));
        let json = std::fs::read_to_string(dir.join("fake").join("transcript.json")).unwrap();
        assert!(json.contains("\"startMs\": 0"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paste_line_reads_as_a_message_with_the_path_last() {
        let md = Path::new("/tmp/demo/clip.md");
        let spoken = paste_line(25_000, Some("Look at the misaligned button."), md);
        assert_eq!(
            spoken,
            "Screen recording (0:25): \"Look at the misaligned button.\" \u{2014} screen frames and video: /tmp/demo/clip.md"
        );
        let silent = paste_line(0, None, md);
        assert_eq!(
            silent,
            "Screen recording, no narration \u{2014} screen frames and video: /tmp/demo/clip.md"
        );
    }

    #[test]
    fn shot_session_paste_is_one_plain_paragraph_with_one_path() {
        let md = Path::new("/tmp/demo/session.md");
        let spoken = shots_paste_line(25_000, Some("Look at the misaligned button."), md);
        assert_eq!(
            spoken,
            "Screen session (0:25): \"Look at the misaligned button.\" \u{2014} screenshots: /tmp/demo/session.md"
        );
        let silent = shots_paste_line(0, None, md);
        assert_eq!(
            silent,
            "Screen session, no narration \u{2014} screenshots: /tmp/demo/session.md"
        );
    }

    #[test]
    fn shot_markdown_pairs_each_image_with_its_sentence() {
        let dir = Path::new("/tmp/demo");
        let shots = [
            Shot {
                at_ms: 1_000,
                path: dir.join("shots/001.png"),
            },
            Shot {
                at_ms: 5_000,
                path: dir.join("shots/002.png"),
            },
        ];
        let shots = shots.iter().collect::<Vec<_>>();
        let segments = [
            segment(0, 4_000, "First thing."),
            segment(4_200, 9_000, "Second thing."),
        ];
        let md = shots_markdown_index(dir, 10_000, &segments, &shots);
        let first = md.find("First thing.").unwrap();
        let shot_one = md.find("shots/001.png").unwrap();
        let second = md.find("Second thing.").unwrap();
        let shot_two = md.find("shots/002.png").unwrap();
        assert!(first < shot_one && shot_one < second && second < shot_two);
    }

    #[test]
    fn timestamps_read_like_a_player() {
        assert_eq!(timestamp(0), "0:00");
        assert_eq!(timestamp(65_000), "1:05");
        assert_eq!(timestamp(3_725_000), "1:02:05");
    }
}
