use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::engine::{Engine, Segment};
use crate::mic::Audio16k;

use super::chunks::{Chunk, Chunker, Tail, RATE, READ};
use super::folder::{self, State, HEARD, MARKDOWN, PREVIOUS, WRITTEN};
use super::heard::{self, Heard};
use super::transcript::{self, Speaker};

/// One chunk gets this many tries, each with the same 30 s of audio at most.
const ATTEMPTS: usize = 3;

pub fn log(folder: &Path, event: &str) {
    use std::io::Write;
    if cfg!(test) {
        return;
    }
    let path = crate::paths::meeting_log();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        // One write per line, so lines from two threads never interleave.
        let line = format!(
            "{}\t{}\t{}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            folder.file_name().unwrap_or_default().to_string_lossy(),
            event
        );
        let _ = file.write_all(line.as_bytes());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Read only the chunks the engine has not read yet.
    Resume,
    /// Read every chunk again, whatever is on record.
    Full,
}

/// The engine the notes are read with. It loads when the first chunk needs
/// it, so a meeting with missing audio or nothing left to read never loads one.
pub struct Reader {
    engine: Option<Box<dyn Engine>>,
    load: fn() -> Result<Box<dyn Engine>, String>,
}

impl Default for Reader {
    fn default() -> Reader {
        Reader {
            engine: None,
            load: || {
                let files = crate::engine::Models::default_root()
                    .ensure(&mut |_| {})
                    .map_err(|error| error.to_string())?;
                crate::engine::load(files).map_err(|error| error.to_string())
            },
        }
    }
}

impl Reader {
    /// Reads with an engine that is already loaded, such as the one the live
    /// lines used.
    pub fn with(engine: Box<dyn Engine>) -> Reader {
        Reader {
            engine: Some(engine),
            ..Reader::default()
        }
    }

    fn engine(&mut self) -> Result<&mut dyn Engine, String> {
        if self.engine.is_none() {
            self.engine = Some((self.load)()?);
        }
        Ok(self.engine.as_deref_mut().expect("loaded above"))
    }
}

/// Transcribe both tracks and write meeting.md.
pub fn write_notes(folder: &Path, reader: &mut Reader, mode: Mode) -> Result<PathBuf, String> {
    let manifest = folder::read(folder).ok_or("meeting manifest missing or invalid")?;
    let tracks = present(folder, &manifest)?;
    let mut local: Vec<PathBuf> = tracks
        .iter()
        .map(|speaker| folder.join(speaker.wav()))
        .collect();
    local.extend([HEARD, MARKDOWN, WRITTEN].map(|name| folder.join(name)));
    if let Some(waited) = fetch(&local)? {
        log(
            folder,
            &format!("downloaded from iCloud in {} s", waited.as_secs()),
        );
    }
    if mode == Mode::Full {
        heard::forget(folder);
    }
    let heard = Heard::read(folder);
    let (mut you, mut them) = (Vec::new(), Vec::new());
    for speaker in tracks {
        let started = Instant::now();
        let read = track(folder, speaker, reader, &heard)?;
        log(
            folder,
            &format!(
                "{} transcribed in {} s: {} chunks read, {} already on record",
                speaker.wav(),
                started.elapsed().as_secs(),
                read.fresh,
                read.reused
            ),
        );
        let placed = transcript::shifted(read.segments, manifest.offset_ms(speaker));
        match speaker {
            Speaker::You => you = placed,
            Speaker::Them => them = placed,
        }
    }
    let lines = transcript::merge(&you, &them);
    publish(folder, &transcript::render(&manifest, &lines)).map_err(|error| error.to_string())
}

/// The tracks to read. A track the recording started and the folder no longer
/// holds fails the meeting by name, so nobody takes notes with one side gone
/// for the whole call.
fn present(folder: &Path, manifest: &folder::Manifest) -> Result<Vec<Speaker>, String> {
    let (found, missing): (Vec<_>, Vec<_>) = [Speaker::You, Speaker::Them]
        .into_iter()
        .filter(|speaker| folder.join(speaker.wav()).is_file() || manifest.expects(*speaker))
        .partition(|speaker| folder.join(speaker.wav()).is_file());
    match missing.as_slice() {
        [] => Ok(found),
        [one] => Err(format!("{} is missing from the meeting folder", one.wav())),
        _ => Err(format!(
            "{} and {} are missing from the meeting folder",
            Speaker::You.wav(),
            Speaker::Them.wav()
        )),
    }
}

/// macOS evicts meeting audio from iCloud Documents when the disk fills. A
/// plain read of an evicted track leaves the download to the kernel, and on
/// 2026-10-27 every read made while iCloud was still evicting the file failed
/// with "Operation timed out". Asking iCloud for the file and waiting until it
/// is local worked in the same state.
fn fetch(paths: &[PathBuf]) -> Result<Option<Duration>, String> {
    let mut left: Vec<_> = paths
        .iter()
        .filter(|path| crate::shadow::evicted(path))
        .cloned()
        .collect();
    let downloading = !left.is_empty();
    let started = Instant::now();
    crate::library::download(&left);
    let deadline = Instant::now() + Duration::from_secs(30 * 60);
    while !left.is_empty() {
        if Instant::now() >= deadline {
            return Err(format!(
                "{} is still downloading from iCloud",
                left[0].file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
        left.retain(|path| crate::shadow::evicted(path));
    }
    Ok(downloading.then(|| started.elapsed()))
}

struct Read {
    /// Timed from the start of the track.
    segments: Vec<Segment>,
    fresh: usize,
    reused: usize,
}

fn track(
    folder: &Path,
    speaker: Speaker,
    reader: &mut Reader,
    heard: &Heard,
) -> Result<Read, String> {
    let name = speaker.wav();
    let mut wav = Tail::new(folder.join(name));
    let mut chunker = Chunker::default();
    let mut read = Read {
        segments: Vec::new(),
        fresh: 0,
        reused: 0,
    };
    loop {
        let samples = wav.read(READ).map_err(|error| format!("{name}: {error}"))?;
        let mut chunks = chunker.push(&samples);
        if samples.is_empty() {
            chunks.extend(chunker.finish());
        }
        for chunk in chunks {
            if let Some(known) = heard.find(speaker, &chunk) {
                read.segments.extend_from_slice(known);
                read.reused += 1;
                continue;
            }
            let segments = hear(reader, &chunk).map_err(|error| {
                format!(
                    "{name} could not be transcribed from {} to {}: {error}",
                    transcript::timestamp(chunk.start_ms()),
                    transcript::timestamp((chunk.end() * 1_000 / RATE) as u64)
                )
            })?;
            if let Err(error) = heard::keep(folder, speaker, &chunk, &segments) {
                log(
                    folder,
                    &format!("could not keep a chunk of {name}: {error}"),
                );
            }
            read.segments.extend(segments);
            read.fresh += 1;
        }
        if samples.is_empty() {
            break;
        }
    }
    if !wav.opened() {
        return Err(format!("{name} is not a recording see.computer can read"));
    }
    Ok(read)
}

/// What one chunk says, timed from the start of its track. A failed pass is
/// tried again on the same chunk, so the engine never sees more than one chunk
/// of audio however often it fails.
fn hear(reader: &mut Reader, chunk: &Chunk) -> Result<Vec<Segment>, String> {
    let audio = Audio16k::from_samples(chunk.samples.clone());
    let mut failure = String::new();
    for _ in 0..ATTEMPTS {
        match reader.engine()?.transcribe(&audio) {
            Ok(said) => return Ok(transcript::shifted(said.segments, chunk.start_ms())),
            Err(error) => failure = error.to_string(),
        }
    }
    Err(failure)
}

/// Writes the finished transcript without losing what the user typed into the
/// last one. Corrections carry over line by line where they still apply. If
/// some could not, or an older meeting has no record of what the app wrote,
/// the corrected file stays in the folder under another name.
fn publish(folder: &Path, fresh: &str) -> io::Result<PathBuf> {
    let _editing = folder::lock_transcript(folder)?;
    let markdown = folder.join(MARKDOWN);
    let current = text_if_present(&markdown)?;
    let written = text_if_present(&folder.join(WRITTEN))?;
    let (text, lost) = match (written.as_deref(), current.as_deref()) {
        (Some(written), Some(current)) => {
            let merged = transcript::carry_corrections(written, current, fresh);
            let lost = (current != written && merged != current).then_some(current);
            (merged, lost)
        }
        (None, Some(current)) if current != fresh => (fresh.to_owned(), Some(current)),
        _ => (fresh.to_owned(), None),
    };
    let next = folder::stage(folder, text.as_bytes())?;
    let baseline = folder::stage(folder, fresh.as_bytes())?;
    if let Some(previous) = lost {
        set_aside(folder, previous)?;
    }
    if text_if_present(&markdown)? != current {
        return Err(io::Error::other(
            "the meeting transcript changed while saving, try again",
        ));
    }
    next.persist(&markdown)?;
    baseline.persist(folder.join(WRITTEN))?;
    Ok(markdown)
}

fn text_if_present(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("{}: {error}", path.display()),
        )),
    }
}

fn set_aside(folder: &Path, text: &str) -> io::Result<()> {
    let mut backup = folder::stage(folder, text.as_bytes())?;
    let (stem, extension) = PREVIOUS.rsplit_once('.').expect("a file name with a dot");
    for index in 1.. {
        let name = if index == 1 {
            PREVIOUS.to_owned()
        } else {
            format!("{stem}-{index}.{extension}")
        };
        let path = folder.join(name);
        match backup.persist_noclobber(&path) {
            Ok(_) => return Ok(()),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                if std::fs::read(path).is_ok_and(|previous| previous == text.as_bytes()) {
                    return Ok(());
                }
                backup = error.file;
            }
            Err(error) => return Err(error.error),
        }
    }
    unreachable!()
}

/// Write the notes and record the result in the manifest.
pub fn transcribe(folder: &Path, reader: &mut Reader, mode: Mode) -> Result<PathBuf, String> {
    if folder::read(folder).is_some_and(|manifest| manifest.state == State::Recording) {
        return Err("Stop recording before transcribing this meeting".to_owned());
    }
    log(folder, "started");
    let started = Instant::now();
    let mut result = write_notes(folder, reader, mode);
    if let Some(mut manifest) =
        folder::read(folder).filter(|m| result.is_ok() || m.state != State::Done)
    {
        match &result {
            Ok(_) => {
                manifest.state = State::Done;
                manifest.error = None;
            }
            Err(error) => {
                manifest.state = State::Failed;
                manifest.error = Some(error.clone());
            }
        }
        if let Err(error) = folder::write(folder, &manifest) {
            result = Err(error.to_string());
        }
    }
    if result.is_ok() {
        heard::forget(folder);
    }
    let seconds = started.elapsed().as_secs();
    match &result {
        Ok(_) => log(folder, &format!("done in {seconds} s")),
        Err(error) => log(folder, &format!("failed after {seconds} s: {error}")),
    }
    result
}

#[cfg(test)]
mod tests;
