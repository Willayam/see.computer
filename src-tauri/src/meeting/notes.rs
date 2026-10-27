use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::engine::{Engine, Segment, Utterance};
use crate::mic::Audio16k;

use super::folder::{self, State, MARKDOWN, THEM, YOU};
use super::transcript;

/// Transcribe both tracks and write meeting.md.
pub fn write_notes(folder: &Path, engine: &mut dyn Engine) -> Result<PathBuf, String> {
    let manifest = folder::read(folder).ok_or("meeting manifest missing or invalid")?;
    fetch(&folder.join(YOU), &folder.join(THEM))?;
    let you = track(&folder.join(YOU), engine)?;
    let them = track(&folder.join(THEM), engine)?;
    let lines = transcript::merge(&you, &them);
    let markdown = folder.join(MARKDOWN);
    std::fs::write(&markdown, transcript::render(&manifest, &lines))
        .map_err(|error| error.to_string())?;
    Ok(markdown)
}

/// macOS evicts meeting audio from iCloud Documents when the disk fills. A
/// plain read of an evicted track leaves the download to the kernel, and on
/// 2026-10-27 every read made while iCloud was still evicting the file failed
/// with "Operation timed out". Asking iCloud for the file and waiting until it
/// is local worked in the same state.
fn fetch(you: &Path, them: &Path) -> Result<(), String> {
    let mut left: Vec<_> = [you, them]
        .into_iter()
        .filter(|path| crate::library::evicted(path))
        .map(Path::to_path_buf)
        .collect();
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
        left.retain(|path| crate::library::evicted(path));
    }
    Ok(())
}

fn track(path: &Path, engine: &mut dyn Engine) -> Result<Vec<Segment>, String> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let mut reader = hound::WavReader::open(path).map_err(|error| error.to_string())?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 || spec.channels != 1 {
        return Err(format!("unexpected WAV format: {}", path.display()));
    }
    let mut utterance = Utterance::default();
    let mut block = Vec::with_capacity(16_000);
    let scale = (1_u64 << spec.bits_per_sample.saturating_sub(1)) as f32;
    match spec.sample_format {
        hound::SampleFormat::Int => {
            for sample in reader.samples::<i32>() {
                block.push(sample.map_err(|error| error.to_string())? as f32 / scale);
                if block.len() == 16_000 {
                    utterance.feed(engine, &Audio16k::from_samples(std::mem::take(&mut block)));
                }
            }
        }
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                block.push(sample.map_err(|error| error.to_string())?);
                if block.len() == 16_000 {
                    utterance.feed(engine, &Audio16k::from_samples(std::mem::take(&mut block)));
                }
            }
        }
    }
    if !block.is_empty() {
        utterance.feed(engine, &Audio16k::from_samples(block));
    }
    if reader.len() == 0 {
        return Ok(Vec::new());
    }
    utterance
        .finish(engine, &Audio16k::from_samples(Vec::new()))
        .map(|result| result.segments)
        .map_err(|error| error.to_string())
}

/// Load an independent engine, write the notes, and record the result in the manifest.
pub fn transcribe(folder: &Path) -> Result<PathBuf, String> {
    let result = (|| {
        let files = crate::engine::Models::default_root()
            .ensure(&mut |_| {})
            .map_err(|error| error.to_string())?;
        let mut engine = crate::engine::load(files).map_err(|error| error.to_string())?;
        write_notes(folder, engine.as_mut())
    })();
    if let Some(mut manifest) = folder::read(folder) {
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
        folder::write(folder, &manifest).map_err(|error| error.to_string())?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineError, Transcription};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fake {
        calls: usize,
        echo: bool,
    }
    impl Engine for Fake {
        fn transcribe(&mut self, _audio: &Audio16k) -> Result<Transcription, EngineError> {
            self.calls += 1;
            let text = if self.echo || self.calls == 1 {
                "Hello"
            } else {
                "Goodbye"
            };
            Ok(Transcription {
                text: None,
                segments: vec![Segment {
                    start_ms: 0,
                    end_ms: 500,
                    text: text.to_owned(),
                }],
                audio: None,
            })
        }
    }
    fn fixture() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "see-meeting-notes-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let folder = folder::create(&root, Some("Zoom"), 1_781_000_000_000).unwrap();
        for name in [YOU, THEM] {
            let mut writer = hound::WavWriter::create(
                folder.join(name),
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 16_000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            for _ in 0..16_000 {
                writer.write_sample(0_i16).unwrap();
            }
            writer.finalize().unwrap();
        }
        let mut manifest = folder::read(&folder).unwrap();
        manifest.ended_ms = Some(manifest.started_ms + 1_000);
        folder::write(&folder, &manifest).unwrap();
        (root, folder)
    }
    #[test]
    fn writes_both_speakers() {
        let (root, folder) = fixture();
        let markdown = write_notes(
            &folder,
            &mut Fake {
                calls: 0,
                echo: false,
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(markdown).unwrap();
        assert!(text.contains("[00:00] You: Hello\n"));
        assert!(text.contains("[00:00] Them: Goodbye\n"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn drops_echo_from_microphone() {
        let (root, folder) = fixture();
        let markdown = write_notes(
            &folder,
            &mut Fake {
                calls: 0,
                echo: true,
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(markdown).unwrap();
        assert!(!text.contains("You: Hello"));
        assert!(text.contains("[00:00] Them: Hello\n"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
