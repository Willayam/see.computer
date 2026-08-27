//! Package a finished take into an agent-readable folder.
//!
//! Next to `<stem>.mov` this writes:
//!
//! ```text
//! <stem>/take.md          the file whose path gets pasted
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

/// Matches the count cap Clips uses for its recommended frames.
pub const MAX_FRAMES: usize = 16;
const MIN_FRAME_GAP_MS: u64 = 3_000;
/// A deliberate in-clip shot has to remain useful on Retina displays; 1280 px
/// made the H.264 source's fidelity loss needlessly more visible.
const FRAME_MAX_WIDTH: i32 = 2_560;
const NEARBY_FRAME_TOLERANCE_MS: i32 = 500;

extern "C" {
    fn sc_clip_duration_seconds(path: *const c_char) -> f64;
    fn sc_frame_jpeg(
        path: *const c_char,
        at_ms: c_longlong,
        max_width: c_int,
        tolerance_ms: c_int,
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

/// What a packaged take folder says about itself, read back from disk. The
/// panel draws its recent rows from this, so the JSON keys stay in one module.
#[derive(Clone)]
pub struct Summary {
    pub markdown: PathBuf,
    /// The narration, when there was any.
    pub text: Option<String>,
    pub screenshot_count: usize,
    pub clip_count: usize,
    pub clip_duration_ms: u64,
}

impl Summary {
    /// The same text the take pasted at the cursor.
    pub fn paste(&self) -> String {
        paste(
            self.text.as_deref(),
            self.screenshot_count,
            self.clip_count,
            self.clip_duration_ms,
            &self.markdown,
        )
    }
}

/// Reads the take folder written next to `<stem>.mov`. `None` when packaging
/// never finished, or the movie predates the folder.
pub fn summary(mov: &Path) -> Option<Summary> {
    let dir = mov.with_extension("");
    if dir == mov {
        return None;
    }
    // Existing installs wrote clip.md, and the tray must keep showing those
    // recordings after new packages switch to take.md.
    let take = dir.join("take.md");
    let markdown = if take.is_file() {
        take
    } else {
        let legacy = dir.join("clip.md");
        legacy.is_file().then_some(legacy)?
    };
    let transcript = std::fs::read_to_string(dir.join("transcript.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or(serde_json::Value::Null);
    let captures = capture_counts(&transcript);
    Some(Summary {
        markdown,
        text: transcript
            .get("fullText")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        screenshot_count: captures.screenshots,
        clip_count: captures.clips,
        clip_duration_ms: captures.clip_duration_ms,
    })
}

#[derive(Clone, Copy)]
struct CaptureCounts {
    screenshots: usize,
    clips: usize,
    clip_duration_ms: u64,
}

fn capture_counts(transcript: &serde_json::Value) -> CaptureCounts {
    let Some(captures) = transcript
        .get("captures")
        .and_then(serde_json::Value::as_array)
    else {
        return CaptureCounts {
            screenshots: 0,
            clips: 1,
            clip_duration_ms: transcript
                .get("durationMs")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        };
    };
    captures.iter().fold(
        CaptureCounts {
            screenshots: 0,
            clips: 0,
            clip_duration_ms: 0,
        },
        |mut counts, capture| {
            match capture.get("type").and_then(serde_json::Value::as_str) {
                Some("shot") => counts.screenshots += 1,
                Some("clip") => {
                    counts.clips += 1;
                    counts.clip_duration_ms = counts.clip_duration_ms.saturating_add(
                        capture
                            .get("durationMs")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                    );
                    counts.screenshots += capture
                        .get("shots")
                        .and_then(serde_json::Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                }
                _ => {}
            }
            counts
        },
    )
}

pub struct Packaged {
    pub markdown: PathBuf,
    /// What lands at the cursor: the spoken words first, so the paste reads as
    /// a message anywhere, then the `take.md` path for agents that can follow it.
    pub paste: String,
}

pub struct Shot {
    pub at_ms: u64,
    pub path: PathBuf,
}

pub struct SessionClip {
    pub start_ms: u64,
    pub end_ms: u64,
    /// The movie starts after the decoder commits the hold, while the logical
    /// clip starts at the finger-down instant.
    pub recording_start_ms: u64,
    pub path: PathBuf,
    pub shots_ms: Vec<u64>,
}

pub enum SessionCapture {
    Shot(Shot),
    Clip(SessionClip),
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("recording has no file stem")]
    NoStem,
    #[error("could not write take folder: {0}")]
    Io(#[from] std::io::Error),
}

pub fn package(mov: &Path, transcription: &Transcription) -> Result<Packaged, PackageError> {
    package_recorded(mov, transcription, None)
}

pub fn package_recorded(
    mov: &Path,
    transcription: &Transcription,
    recorded_duration_ms: Option<u64>,
) -> Result<Packaged, PackageError> {
    let dir = mov.with_extension("");
    if dir == mov || mov.file_stem().is_none() {
        return Err(PackageError::NoStem);
    }
    let frames_dir = dir.join("frames");
    let _ = std::fs::remove_dir_all(&frames_dir);
    std::fs::create_dir_all(&frames_dir)?;

    let duration_ms = duration_ms(mov, &transcription.segments, recorded_duration_ms);
    let mut frames = Vec::new();
    for at in frame_times(duration_ms, &transcription.segments) {
        let file = frames_dir.join(format!("{at:07}.jpg"));
        if extract_frame(mov, at, NEARBY_FRAME_TOLERANCE_MS, &file) {
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
    let markdown = dir.join("take.md");
    std::fs::write(
        &markdown,
        markdown_index(&mov_name, duration_ms, &transcription.segments, &frames),
    )?;
    let paste = paste(
        transcription.text.as_ref().map(|text| text.as_str()),
        0,
        1,
        duration_ms,
        &markdown,
    );
    Ok(Packaged { markdown, paste })
}

/// The empty line keeps the narration as the message and the take path as a
/// supporting note, which is the sparse shape that made agents inspect selectively.
pub fn paste(
    full_text: Option<&str>,
    screenshot_count: usize,
    clip_count: usize,
    clip_duration_ms: u64,
    markdown: &Path,
) -> String {
    let mut kinds = Vec::with_capacity(2);
    match screenshot_count {
        0 => {}
        1 => kinds.push("1 screenshot".to_owned()),
        count => kinds.push(format!("{count} screenshots")),
    }
    match clip_count {
        0 => {}
        1 => kinds.push(format!("1 clip ({})", timestamp(clip_duration_ms))),
        count => kinds.push(format!("{count} clips ({})", timestamp(clip_duration_ms))),
    }
    let tail = kinds.join(", ");
    match full_text {
        Some(text) if !text.is_empty() => {
            format!("\"{text}\"\n\n{tail}: {}", markdown.display())
        }
        _ => format!("No narration.\n\n{tail}: {}", markdown.display()),
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
    let markdown = dir.join("take.md");
    std::fs::write(
        &markdown,
        shots_markdown_index(dir, duration_ms, &transcription.segments, &shots),
    )?;
    let paste = paste(
        transcription.text.as_ref().map(|text| text.as_str()),
        shots.len(),
        0,
        0,
        &markdown,
    );
    Ok(Packaged { markdown, paste })
}

pub fn package_session(
    dir: &Path,
    duration_ms: u64,
    transcription: &Transcription,
    captures: &[SessionCapture],
) -> Result<Packaged, PackageError> {
    let frames_dir = dir.join("frames");
    let shots_dir = dir.join("shots");
    let _ = std::fs::remove_dir_all(&frames_dir);
    std::fs::create_dir_all(&frames_dir)?;
    std::fs::create_dir_all(&shots_dir)?;

    let duration_ms = duration_ms.max(
        transcription
            .segments
            .last()
            .map(|segment| segment.end_ms)
            .unwrap_or(0),
    );
    let mut artifacts = Vec::new();
    let mut json_captures = Vec::new();
    let mut clip_number = 0;
    let mut shot_number = 0;
    let mut screenshot_count = 0;
    let mut clip_duration_ms = 0_u64;

    for capture in captures {
        match capture {
            SessionCapture::Shot(shot) => {
                shot_number += 1;
                screenshot_count += 1;
                let file = path_from(dir, &shot.path);
                artifacts.push(Artifact::image(shot.at_ms, file.clone()));
                json_captures.push(serde_json::json!({
                    "type": "shot",
                    "atMs": shot.at_ms,
                    "timestamp": timestamp(shot.at_ms),
                    "file": file,
                }));
            }
            SessionCapture::Clip(clip) => {
                clip_number += 1;
                clip_duration_ms =
                    clip_duration_ms.saturating_add(clip.end_ms.saturating_sub(clip.start_ms));
                let video = path_from(dir, &clip.path);
                artifacts.push(Artifact::clip(
                    clip.start_ms,
                    clip.end_ms.saturating_sub(clip.start_ms),
                    video.clone(),
                ));

                let readable_start = clip.recording_start_ms.max(clip.start_ms);
                let readable_duration = clip.end_ms.saturating_sub(readable_start);
                let local_segments = transcription
                    .segments
                    .iter()
                    .filter(|segment| {
                        segment.end_ms > readable_start && segment.start_ms < clip.end_ms
                    })
                    .map(|segment| Segment {
                        start_ms: segment.start_ms.saturating_sub(readable_start),
                        end_ms: segment
                            .end_ms
                            .min(clip.end_ms)
                            .saturating_sub(readable_start),
                        text: segment.text.clone(),
                    })
                    .collect::<Vec<_>>();
                let mut frames = Vec::new();
                for local_at in frame_times(readable_duration, &local_segments) {
                    let session_at = readable_start.saturating_add(local_at);
                    let file =
                        frames_dir.join(format!("clip-{clip_number:03}-{session_at:07}.jpg"));
                    if extract_frame(&clip.path, local_at, NEARBY_FRAME_TOLERANCE_MS, &file) {
                        let relative = path_from(dir, &file);
                        artifacts.push(Artifact::image(session_at, relative.clone()));
                        frames.push(serde_json::json!({
                            "atMs": session_at,
                            "timestamp": timestamp(session_at),
                            "file": relative,
                        }));
                    }
                }

                let mut shots = Vec::new();
                for at_ms in &clip.shots_ms {
                    shot_number += 1;
                    let file = shots_dir.join(format!("{shot_number:03}.jpg"));
                    let local_at = at_ms.saturating_sub(clip.recording_start_ms);
                    if extract_frame(&clip.path, local_at, 0, &file) {
                        screenshot_count += 1;
                        let relative = path_from(dir, &file);
                        artifacts.push(Artifact::image(*at_ms, relative.clone()));
                        shots.push(serde_json::json!({
                            "atMs": at_ms,
                            "timestamp": timestamp(*at_ms),
                            "file": relative,
                        }));
                    }
                }
                json_captures.push(serde_json::json!({
                    "type": "clip",
                    "startMs": clip.start_ms,
                    "endMs": clip.end_ms,
                    "durationMs": clip.end_ms.saturating_sub(clip.start_ms),
                    "video": video,
                    "frames": frames,
                    "shots": shots,
                }));
            }
        }
    }

    artifacts.sort_by_key(|artifact| artifact.at_ms);
    json_captures.sort_by_key(|capture| {
        capture
            .get("atMs")
            .or_else(|| capture.get("startMs"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    });
    std::fs::write(
        dir.join("transcript.json"),
        session_transcript_json(duration_ms, transcription, json_captures),
    )?;
    let markdown = dir.join("take.md");
    std::fs::write(
        &markdown,
        session_markdown_index(duration_ms, &transcription.segments, &artifacts),
    )?;
    let paste = paste(
        transcription.text.as_ref().map(|text| text.as_str()),
        screenshot_count,
        clip_number,
        clip_duration_ms,
        &markdown,
    );
    Ok(Packaged { markdown, paste })
}

pub fn package_single_clip(
    duration_ms: u64,
    transcription: &Transcription,
    clip: SessionClip,
) -> Result<Packaged, PackageError> {
    let dir = clip.path.with_extension("");
    if dir == clip.path || clip.path.file_stem().is_none() {
        return Err(PackageError::NoStem);
    }
    let packaged = package_session(
        &dir,
        duration_ms,
        transcription,
        &[SessionCapture::Clip(clip)],
    )?;
    let _ = std::fs::remove_dir(dir.join("shots"));
    Ok(packaged)
}

struct Artifact {
    at_ms: u64,
    markdown: String,
}

impl Artifact {
    fn image(at_ms: u64, file: String) -> Artifact {
        Artifact {
            at_ms,
            markdown: format!("![{}]({file})", timestamp(at_ms)),
        }
    }

    fn clip(at_ms: u64, duration_ms: u64, file: String) -> Artifact {
        Artifact {
            at_ms,
            markdown: format!(
                "[Video clip at {} ({} long)]({file})",
                timestamp(at_ms),
                timestamp(duration_ms)
            ),
        }
    }
}

fn path_from(dir: &Path, path: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(dir) {
        return relative.to_string_lossy().into_owned();
    }
    if dir.parent() == path.parent() {
        return path
            .file_name()
            .map(|file| format!("../{}", file.to_string_lossy()))
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
    }
    path.to_string_lossy().into_owned()
}

fn session_markdown_index(
    duration_ms: u64,
    segments: &[Segment],
    artifacts: &[Artifact],
) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Screen take\n");
    let length = if duration_ms > 0 {
        format!("{} long, ", timestamp(duration_ms))
    } else {
        String::new()
    };
    let _ = writeln!(
        md,
        "{length}captured with see.computer. The transcript below is timestamped, and every \
         screenshot, video, and extracted frame sits with the sentence being spoken when it \
         was captured. The timings and paths are also in \
         [`transcript.json`](transcript.json).\n"
    );
    let _ = writeln!(md, "## Transcript\n");
    if segments.is_empty() {
        let _ = writeln!(md, "No speech was detected.\n");
        for artifact in artifacts {
            let _ = writeln!(md, "{}\n", artifact.markdown);
        }
        return md;
    }

    let mut remaining = artifacts.iter().peekable();
    for segment in segments {
        while let Some(artifact) = remaining.peek() {
            if artifact.at_ms < segment.start_ms {
                let _ = writeln!(md, "{}\n", artifact.markdown);
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
        while let Some(artifact) = remaining.peek() {
            if artifact.at_ms < segment.end_ms {
                let _ = writeln!(md, "{}\n", artifact.markdown);
                remaining.next();
            } else {
                break;
            }
        }
    }
    for artifact in remaining {
        let _ = writeln!(md, "{}\n", artifact.markdown);
    }
    md
}

fn session_transcript_json(
    duration_ms: u64,
    transcription: &Transcription,
    captures: Vec<serde_json::Value>,
) -> String {
    let segments = transcription
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
        .collect::<Vec<_>>();
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

fn shots_markdown_index(
    dir: &Path,
    duration_ms: u64,
    segments: &[Segment],
    shots: &[&Shot],
) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Screen take\n");
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

fn extract_frame(mov: &Path, at_ms: u64, tolerance_ms: i32, out: &Path) -> bool {
    let (Some(mov), Some(out)) = (c_path(mov), c_path(out)) else {
        return false;
    };
    unsafe {
        sc_frame_jpeg(
            mov.as_ptr(),
            at_ms as c_longlong,
            FRAME_MAX_WIDTH,
            tolerance_ms,
            out.as_ptr(),
        ) == 0
    }
}

fn duration_ms(mov: &Path, segments: &[Segment], recorded_duration_ms: Option<u64>) -> u64 {
    let native = c_path(mov)
        .map(|path| unsafe { sc_clip_duration_seconds(path.as_ptr()) })
        .unwrap_or(-1.0);
    preferred_duration_ms(recorded_duration_ms, native, segments)
}

fn preferred_duration_ms(
    recorded_duration_ms: Option<u64>,
    native_seconds: f64,
    segments: &[Segment],
) -> u64 {
    if let Some(recorded) = recorded_duration_ms {
        return recorded;
    }
    if native_seconds > 0.0 {
        return (native_seconds * 1000.0).round() as u64;
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
    let _ = writeln!(md, "# Screen take\n");
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
        assert_eq!(packaged.markdown, dir.join("fake").join("take.md"));
        let md = std::fs::read_to_string(&packaged.markdown).unwrap();
        assert!(md.contains("Hello there."));
        let json = std::fs::read_to_string(dir.join("fake").join("transcript.json")).unwrap();
        assert!(json.contains("\"startMs\": 0"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paste_names_each_capture_combination() {
        let markdown = Path::new("/tmp/demo/take.md");
        let cases = [
            (1, 0, 0, "1 screenshot"),
            (2, 0, 0, "2 screenshots"),
            (0, 1, 2_000, "1 clip (0:02)"),
            (0, 2, 47_000, "2 clips (0:47)"),
            (2, 1, 2_000, "2 screenshots, 1 clip (0:02)"),
            (3, 2, 72_000, "3 screenshots, 2 clips (1:12)"),
        ];
        for (screenshots, clips, duration_ms, tail) in cases {
            assert_eq!(
                paste(
                    Some("Look at the misaligned button."),
                    screenshots,
                    clips,
                    duration_ms,
                    markdown,
                ),
                format!("\"Look at the misaligned button.\"\n\n{tail}: /tmp/demo/take.md")
            );
        }
    }

    #[test]
    fn paste_omits_duration_without_a_clip_and_drops_empty_quotes() {
        let markdown = Path::new("/tmp/demo/take.md");
        let stills = paste(Some("Narration."), 2, 0, 99_000, markdown);
        assert_eq!(stills, "\"Narration.\"\n\n2 screenshots: /tmp/demo/take.md");
        assert!(!stills.contains('('));

        let silent = paste(None, 2, 1, 2_000, markdown);
        assert_eq!(
            silent,
            "No narration.\n\n2 screenshots, 1 clip (0:02): /tmp/demo/take.md"
        );
    }

    #[test]
    fn summary_reads_a_legacy_clip_folder() {
        let root = std::env::temp_dir().join(format!(
            "see-computer-legacy-summary-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mov = root.join("legacy.mov");
        let dir = mov.with_extension("");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&mov, b"not a movie").unwrap();
        std::fs::write(dir.join("clip.md"), "# Legacy recording").unwrap();
        std::fs::write(
            dir.join("transcript.json"),
            r#"{"durationMs": 25000, "fullText": "Keep this recent."}"#,
        )
        .unwrap();

        let summary = summary(&mov).expect("legacy clip should remain in recents");
        assert_eq!(summary.markdown, dir.join("clip.md"));
        assert_eq!(
            summary.paste(),
            format!(
                "\"Keep this recent.\"\n\n1 clip (0:25): {}",
                dir.join("clip.md").display()
            )
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn summary_prefers_take_and_rebuilds_its_capture_tail() {
        let root = std::env::temp_dir().join(format!(
            "see-computer-take-summary-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mov = root.join("current.mov");
        let dir = mov.with_extension("");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&mov, b"not a movie").unwrap();
        std::fs::write(dir.join("take.md"), "# Current take").unwrap();
        std::fs::write(dir.join("clip.md"), "# Stale legacy take").unwrap();
        std::fs::write(
            dir.join("transcript.json"),
            r#"{
                "fullText": "Inspect this.",
                "captures": [
                    {"type": "shot"},
                    {"type": "shot"},
                    {"type": "clip", "durationMs": 2000, "shots": []}
                ]
            }"#,
        )
        .unwrap();

        let summary = summary(&mov).expect("take should appear in recents");
        assert_eq!(summary.markdown, dir.join("take.md"));
        assert_eq!(
            summary.paste(),
            format!(
                "\"Inspect this.\"\n\n2 screenshots, 1 clip (0:02): {}",
                dir.join("take.md").display()
            )
        );
        std::fs::remove_dir_all(root).unwrap();
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
    fn a_recorded_end_wins_over_the_movies_tail() {
        let segments = [segment(0, 1_500, "The transcript is longer too.")];
        assert_eq!(preferred_duration_ms(Some(800), 2.4, &segments), 800);
    }

    #[test]
    fn mixed_session_keeps_one_plain_paste_and_interleaves_captures() {
        let dir = std::env::temp_dir().join(format!(
            "see-computer-mixed-clip-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shots")).unwrap();
        let shot = dir.join("shots/001.png");
        std::fs::write(&shot, b"screenshot").unwrap();
        let mov = dir.with_extension("mov");
        std::fs::write(&mov, b"not a movie").unwrap();
        let transcription = Transcription {
            text: Text::parse("First thing. Second thing."),
            segments: vec![
                segment(0, 4_000, "First thing."),
                segment(4_000, 8_000, "Second thing."),
            ],
        };
        let captures = [
            SessionCapture::Shot(Shot {
                at_ms: 1_000,
                path: shot,
            }),
            SessionCapture::Clip(SessionClip {
                start_ms: 5_000,
                end_ms: 7_000,
                recording_start_ms: 5_250,
                path: mov.clone(),
                shots_ms: Vec::new(),
            }),
        ];

        let packaged = package_session(&dir, 8_000, &transcription, &captures).unwrap();
        assert_eq!(
            packaged.paste,
            format!(
                "\"First thing. Second thing.\"\n\n1 screenshot, 1 clip (0:02): {}",
                dir.join("take.md").display()
            )
        );
        let markdown = std::fs::read_to_string(packaged.markdown).unwrap();
        let first = markdown.find("First thing.").unwrap();
        let shot = markdown.find("shots/001.png").unwrap();
        let second = markdown.find("Second thing.").unwrap();
        let clip = markdown.find("Video clip at 0:05").unwrap();
        assert!(first < shot && shot < second && second < clip);

        std::fs::remove_file(mov).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn one_clip_without_shots_keeps_the_flat_clip_layout() {
        let root = std::env::temp_dir().join(format!(
            "see-computer-flat-session-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mov = root.join("capture.mov");
        std::fs::write(&mov, b"not a movie").unwrap();
        let transcription = Transcription {
            text: Text::parse("Keep the common layout."),
            segments: vec![segment(0, 1_000, "Keep the common layout.")],
        };

        let packaged = package_single_clip(
            1_000,
            &transcription,
            SessionClip {
                start_ms: 0,
                end_ms: 1_000,
                recording_start_ms: 250,
                path: mov.clone(),
                shots_ms: Vec::new(),
            },
        )
        .unwrap();

        assert_eq!(packaged.markdown, root.join("capture/take.md"));
        assert!(mov.is_file());
        assert!(root.join("capture/frames").is_dir());
        assert!(root.join("capture/transcript.json").is_file());
        assert!(!root.join("capture/clip.md").exists());
        assert!(!root.join("capture/session.md").exists());
        assert!(!root.join("capture/shots").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timestamps_read_like_a_player() {
        assert_eq!(timestamp(0), "0:00");
        assert_eq!(timestamp(65_000), "1:05");
        assert_eq!(timestamp(3_725_000), "1:02:05");
    }
}
