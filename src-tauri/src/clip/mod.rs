//! Package a finished take into an agent-readable folder.
//!
//! Each take owns its movies, screenshots, and extracted frames:
//!
//! ```text
//! <take>/take.md
//! <take>/transcript.json
//! <take>/shots/001.png
//! <take>/clips/001/clip.mov
//! <take>/clips/001/frames/<ms>.jpg
//! ```
//!
//! Clips use recording order for their directory number. The markdown
//! interleaves captures with the timestamped transcript so an agent can read
//! what was said and open the screen exactly where it was said.

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

/// Splits a pasted string back into what the pill shows: the words that were
/// said, and the short note about what was captured. The inverse of `paste`, so
/// the two must change together; the test below holds them to that.
pub fn describe(pasted: &str) -> crate::pill::Held {
    match pasted.split_once("\n\n") {
        Some((spoken, tail)) => crate::pill::Held {
            text: spoken.trim().trim_matches('"').to_owned(),
            note: tail
                .rsplit_once(": ")
                .map(|(kinds, _)| kinds.trim().to_owned())
                .filter(|kinds| !kinds.is_empty()),
            clipboard: pasted.to_owned(),
        },
        // A plain dictation is only ever the words.
        None => crate::pill::Held {
            text: pasted.trim().to_owned(),
            note: None,
            clipboard: pasted.to_owned(),
        },
    }
}

/// Reads a nested take directory or a legacy movie with its sibling folder.
/// `None` when packaging never finished.
pub fn summary(path: &Path) -> Option<Summary> {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        let dir = path.with_extension("");
        if dir == path {
            return None;
        }
        dir
    };
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
        1 => kinds.push(format!("1 clip ({})", duration_timestamp(clip_duration_ms))),
        count => kinds.push(format!(
            "{count} clips ({})",
            duration_timestamp(clip_duration_ms)
        )),
    }
    let tail = kinds.join(", ");
    match full_text {
        Some(text) if !text.is_empty() => {
            format!("\"{text}\"\n\n{tail}: {}", markdown.display())
        }
        _ => format!("No narration.\n\n{tail}: {}", markdown.display()),
    }
}

pub fn package_session(
    dir: &Path,
    duration_ms: u64,
    transcription: &Transcription,
    captures: &[SessionCapture],
) -> Result<Packaged, PackageError> {
    package_session_with(dir, duration_ms, transcription, captures, extract_frame)
}

fn package_session_with(
    dir: &Path,
    duration_ms: u64,
    transcription: &Transcription,
    captures: &[SessionCapture],
    extract: impl Fn(&Path, u64, i32, &Path) -> bool,
) -> Result<Packaged, PackageError> {
    let shots_dir = dir.join("shots");
    let _ = std::fs::remove_dir_all(dir.join("frames"));
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
    let mut clip_duration_ms = 0_u64;
    let mut movies = Vec::new();

    let mut captures = captures.iter().collect::<Vec<_>>();
    captures.sort_by_key(|capture| match capture {
        SessionCapture::Shot(shot) => shot.at_ms,
        SessionCapture::Clip(clip) => clip.start_ms,
    });
    for capture in captures {
        match capture {
            SessionCapture::Shot(shot) => {
                shot_number += 1;
                let path = place_shot(dir, &shot.path, shot_number)?;
                let file = path_from(dir, &path);
                artifacts.push(Artifact::image(shot.at_ms, file.clone()));
                json_captures.push(serde_json::json!({
                    "type": "shot",
                    "atMs": shot.at_ms,
                    "timestamp": position_timestamp(shot.at_ms),
                    "file": file,
                }));
            }
            SessionCapture::Clip(clip) => {
                clip_number += 1;
                clip_duration_ms =
                    clip_duration_ms.saturating_add(clip.end_ms.saturating_sub(clip.start_ms));
                let clip_dir = dir.join(format!("clips/{clip_number:03}"));
                let frames_dir = clip_dir.join("frames");
                std::fs::create_dir_all(&frames_dir)?;
                let nested_mov = clip_dir.join("clip.mov");
                let video = path_from(dir, &nested_mov);
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
                    let file = frames_dir.join(format!("{session_at:07}.jpg"));
                    if extract(&clip.path, local_at, NEARBY_FRAME_TOLERANCE_MS, &file) {
                        let relative = path_from(dir, &file);
                        artifacts.push(Artifact::image(session_at, relative.clone()));
                        frames.push(serde_json::json!({
                            "atMs": session_at,
                            "timestamp": position_timestamp(session_at),
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
                }));
                movies.push((&clip.path, nested_mov));
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
    for (source, destination) in movies {
        move_file(source, &destination)?;
    }
    let paste = paste(
        transcription.text.as_ref().map(|text| text.as_str()),
        shot_number,
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

/// Package everything a take captured. A take with its own folder is packaged
/// there; a take that never got one nests its first clip beside the movie.
pub fn package_take(
    dir: Option<PathBuf>,
    duration_ms: u64,
    transcription: &Transcription,
    captures: Vec<SessionCapture>,
) -> Result<Packaged, PackageError> {
    match dir {
        Some(dir) => package_session(&dir, duration_ms, transcription, &captures),
        None => match captures.into_iter().next() {
            Some(SessionCapture::Clip(clip)) => {
                package_single_clip(duration_ms, transcription, clip)
            }
            _ => Err(PackageError::NoStem),
        },
    }
}

struct Artifact {
    at_ms: u64,
    markdown: String,
}

impl Artifact {
    fn image(at_ms: u64, file: String) -> Artifact {
        Artifact {
            at_ms,
            markdown: format!("![{}]({file})", position_timestamp(at_ms)),
        }
    }

    fn clip(at_ms: u64, duration_ms: u64, file: String) -> Artifact {
        Artifact {
            at_ms,
            markdown: format!(
                "[Video clip at {} ({} long)]({file})",
                position_timestamp(at_ms),
                duration_timestamp(duration_ms)
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

fn move_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    if source == destination {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(source, destination)
}

fn place_shot(dir: &Path, source: &Path, number: usize) -> std::io::Result<PathBuf> {
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("png");
    let destination = dir.join("shots").join(format!("{number:03}.{extension}"));
    move_file(source, &destination)?;
    Ok(destination)
}

fn session_markdown_index(
    duration_ms: u64,
    segments: &[Segment],
    artifacts: &[Artifact],
) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Screen take\n");
    let length = if duration_ms > 0 {
        format!("{} long, ", duration_timestamp(duration_ms))
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
            position_timestamp(segment.start_ms),
            position_timestamp(segment.end_ms),
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
                "range": format!(
                    "{}-{}",
                    position_timestamp(segment.start_ms),
                    position_timestamp(segment.end_ms)
                ),
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

pub fn movie_duration_ms(mov: &Path, segments: &[Segment]) -> u64 {
    let native = c_path(mov)
        .map(|path| unsafe { sc_clip_duration_seconds(path.as_ptr()) })
        .unwrap_or(-1.0);
    preferred_duration_ms(native, segments)
}

fn preferred_duration_ms(native_seconds: f64, segments: &[Segment]) -> u64 {
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

// Durations round to report the nearest useful length. Positions floor so a
// marker never jumps ahead of the words or frame it labels.
fn duration_timestamp(ms: u64) -> String {
    format_seconds(ms.saturating_add(500) / 1000)
}

fn position_timestamp(ms: u64) -> String {
    format_seconds(ms / 1000)
}

fn format_seconds(total: u64) -> String {
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests;
