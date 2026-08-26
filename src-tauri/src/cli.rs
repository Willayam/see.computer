//! `see-computer transcribe <wav>` uses the same engine and model root as the app.

use std::path::PathBuf;
use std::time::Instant;

pub enum Cmd {
    Transcribe(PathBuf),
}

pub fn parse(mut args: impl Iterator<Item = String>) -> Option<Cmd> {
    match args.next().as_deref() {
        Some("transcribe") => args.next().map(PathBuf::from).map(Cmd::Transcribe),
        _ => None,
    }
}

pub fn run(cmd: Cmd) -> i32 {
    let Cmd::Transcribe(path) = cmd;
    let total_start = Instant::now();
    let audio = match crate::mic::Audio16k::from_wav(&path) {
        Ok(audio) => audio,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    eprintln!("audio: {:.3}s", audio.seconds());
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
    // Same thread class the app's engine worker runs at, so the numbers this
    // prints are the numbers a dictation gets, contention included.
    let loaded = crate::qos::spawn("see-engine", crate::qos::Class::Engine, move || {
        let mut engine = crate::engine::load(files).map_err(|error| error.to_string())?;
        let inference_start = Instant::now();
        let result = engine.transcribe(&audio);
        Ok::<_, String>((result, inference_start.elapsed()))
    })
    .join();
    let (result, inference) = match loaded {
        Ok(Ok(pair)) => pair,
        Ok(Err(error)) => {
            eprintln!("{error}");
            return 1;
        }
        Err(_) => {
            eprintln!("transcription thread panicked");
            return 1;
        }
    };
    eprintln!("transcription: {:.3} ms", inference.as_secs_f64() * 1_000.0);
    eprintln!(
        "wall: {:.3} ms",
        total_start.elapsed().as_secs_f64() * 1_000.0
    );
    match result {
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
