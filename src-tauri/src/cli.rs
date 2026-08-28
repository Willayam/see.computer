//! `see-computer transcribe <wav>` and `see-computer clip <mov>` use the same
//! engine and model root as the app.

use std::path::PathBuf;
use std::time::Instant;

use crate::engine::{EngineError, Transcription};

pub enum Cmd {
    Transcribe(PathBuf),
    TranscribeLive(PathBuf),
    Clip(PathBuf),
}

pub fn parse(mut args: impl Iterator<Item = String>) -> Option<Cmd> {
    match args.next().as_deref() {
        Some("transcribe") => match args.next().as_deref() {
            Some("--live") => args.next().map(PathBuf::from).map(Cmd::TranscribeLive),
            Some(path) => Some(Cmd::Transcribe(PathBuf::from(path))),
            None => None,
        },
        Some("clip") => args.next().map(PathBuf::from).map(Cmd::Clip),
        _ => None,
    }
}

pub fn run(cmd: Cmd) -> i32 {
    match cmd {
        Cmd::Transcribe(path) => transcribe(path),
        Cmd::TranscribeLive(path) => transcribe_live(path),
        Cmd::Clip(path) => clip(path),
    }
}

fn transcribe_live(path: PathBuf) -> i32 {
    let audio = match crate::mic::Audio16k::from_wav(&path) {
        Ok(audio) => audio,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    eprintln!("audio: {:.3}s", audio.seconds());
    let split = audio
        .samples()
        .len()
        .saturating_sub(15 * crate::mic::RATE as usize);
    let head = audio.samples()[..split].to_vec();
    let tail = crate::mic::Audio16k::from_samples(audio.samples()[split..].to_vec());
    let models = crate::engine::Models::default_root();
    let files = match models.ensure(&mut |progress| {
        if let Some(percent) = progress.percent() {
            eprintln!("model: {percent}%");
        }
    }) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    eprintln!("model: loading and warming");
    let loaded = crate::qos::spawn("see-engine", crate::qos::Class::Engine, move || {
        let mut engine = crate::engine::load(files).map_err(|error| error.to_string())?;
        let mut utterance = crate::engine::Utterance::default();
        for block in head.chunks(crate::mic::RATE as usize / 5) {
            utterance.feed(
                engine.as_mut(),
                &crate::mic::Audio16k::from_samples(block.to_vec()),
            );
        }
        let inference_start = Instant::now();
        let result = utterance.finish(engine.as_mut(), &tail);
        Ok::<_, String>((result, inference_start.elapsed()))
    })
    .join();
    let result = match loaded {
        Ok(Ok((result, inference))) => {
            eprintln!(
                "tail transcription: {:.3} ms",
                inference.as_secs_f64() * 1_000.0
            );
            result
        }
        Ok(Err(error)) => {
            eprintln!("{error}");
            return 1;
        }
        Err(_) => {
            eprintln!("transcription thread panicked");
            return 1;
        }
    };
    match result.map(|transcription| transcription.text) {
        Ok(Some(text)) => {
            println!("{}", text.as_str());
            0
        }
        Ok(None) => {
            eprintln!("nothing heard");
            2
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn transcribe(path: PathBuf) -> i32 {
    let total_start = Instant::now();
    let audio = match crate::mic::Audio16k::from_wav(&path) {
        Ok(audio) => audio,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    eprintln!("audio: {:.3}s", audio.seconds());
    let result = match transcribe_on_engine_thread(audio) {
        Ok(result) => result,
        Err(code) => return code,
    };
    eprintln!(
        "wall: {:.3} ms",
        total_start.elapsed().as_secs_f64() * 1_000.0
    );
    match result.map(|transcription| transcription.text) {
        Ok(Some(text)) => {
            println!("{}", text.as_str());
            0
        }
        Ok(None) => {
            eprintln!("nothing heard");
            2
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

/// Rebuild the agent-readable folder for a finished recording and print the
/// path of its `take.md`.
fn clip(mov: PathBuf) -> i32 {
    if !mov.exists() {
        eprintln!("no such file: {}", mov.display());
        return 1;
    }
    let total_start = Instant::now();
    let transcription = match crate::clip::extract_audio(&mov) {
        None => {
            eprintln!("audio: none readable; packaging frames only");
            Transcription::empty()
        }
        Some(audio) => {
            eprintln!("audio: {:.3}s", audio.seconds());
            match transcribe_on_engine_thread(audio) {
                Err(code) => return code,
                Ok(Err(error)) => {
                    eprintln!("{error}");
                    return 1;
                }
                Ok(Ok(transcription)) => {
                    eprintln!("segments: {}", transcription.segments.len());
                    transcription
                }
            }
        }
    };
    match crate::clip::package(&mov, &transcription) {
        Ok(packaged) => {
            eprintln!(
                "wall: {:.3} ms",
                total_start.elapsed().as_secs_f64() * 1_000.0
            );
            println!("{}", packaged.markdown.display());
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

/// Download and load the model, then transcribe on a thread with the same
/// class the app's engine worker runs at, so the numbers this prints are the
/// numbers a dictation gets, contention included.
fn transcribe_on_engine_thread(
    audio: crate::mic::Audio16k,
) -> Result<Result<Transcription, EngineError>, i32> {
    let models = crate::engine::Models::default_root();
    let files = match models.ensure(&mut |progress| {
        if let Some(percent) = progress.percent() {
            eprintln!("model: {percent}%");
        }
    }) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{error}");
            return Err(1);
        }
    };
    eprintln!("model: loading and warming");
    let loaded = crate::qos::spawn("see-engine", crate::qos::Class::Engine, move || {
        let mut engine = crate::engine::load(files).map_err(|error| error.to_string())?;
        let inference_start = Instant::now();
        let result = engine.transcribe(&audio);
        Ok::<_, String>((result, inference_start.elapsed()))
    })
    .join();
    match loaded {
        Ok(Ok((result, inference))) => {
            eprintln!("transcription: {:.3} ms", inference.as_secs_f64() * 1_000.0);
            Ok(result)
        }
        Ok(Err(error)) => {
            eprintln!("{error}");
            Err(1)
        }
        Err(_) => {
            eprintln!("transcription thread panicked");
            Err(1)
        }
    }
}
