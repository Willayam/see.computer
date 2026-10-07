//! `see-computer transcribe <wav>`, `clip <mov>`, `meetings`, `notes <meeting-folder>`, and `tap <seconds> <out.wav> [bundle-id]` are CLI commands.
//! The first two use the same engine and model root as the app.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::engine::{EngineError, Transcription};

pub enum Cmd {
    Transcribe(PathBuf),
    TranscribeLive(PathBuf),
    Clip(PathBuf),
    Meetings,
    Notes(PathBuf),
    Tap(u64, PathBuf, Option<String>),
}

pub fn parse(mut args: impl Iterator<Item = String>) -> Option<Cmd> {
    match args.next().as_deref() {
        Some("transcribe") => match args.next().as_deref() {
            Some("--live") => args.next().map(PathBuf::from).map(Cmd::TranscribeLive),
            Some(path) => Some(Cmd::Transcribe(PathBuf::from(path))),
            None => None,
        },
        Some("clip") => args.next().map(PathBuf::from).map(Cmd::Clip),
        Some("meetings") => Some(Cmd::Meetings),
        Some("notes") => args.next().map(PathBuf::from).map(Cmd::Notes),
        Some("tap") => {
            let seconds = args.next()?.parse().ok()?;
            let path = PathBuf::from(args.next()?);
            Some(Cmd::Tap(seconds, path, args.next()))
        }
        _ => None,
    }
}

pub fn run(cmd: Cmd) -> i32 {
    match cmd {
        Cmd::Transcribe(path) => transcribe(path),
        Cmd::TranscribeLive(path) => transcribe_live(path),
        Cmd::Clip(path) => clip(path),
        Cmd::Meetings => meetings(),
        Cmd::Notes(folder) => match crate::meeting::notes::transcribe(&folder) {
            Ok(path) => {
                println!("{}", path.display());
                0
            }
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        Cmd::Tap(seconds, path, bundle) => tap(seconds, path, bundle),
    }
}

fn tap(seconds: u64, path: PathBuf, bundle: Option<String>) -> i32 {
    let objects = if let Some(bundle) = bundle {
        let processes = match crate::meeting::holders::processes() {
            Ok(processes) => processes,
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        };
        let objects = processes
            .into_iter()
            .filter(|process| {
                process.bundle_id.as_deref().is_some_and(|id| {
                    id == bundle
                        || id
                            .strip_prefix(&bundle)
                            .is_some_and(|rest| rest.starts_with('.'))
                })
            })
            .map(|process| process.object)
            .collect::<Vec<_>>();
        if objects.is_empty() {
            eprintln!("no audio processes match {bundle}");
            return 1;
        }
        objects
    } else {
        Vec::new()
    };
    let recording = match crate::meeting::tap::start(&objects, &path) {
        Ok(recording) => recording,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    std::thread::sleep(Duration::from_secs(seconds));
    drop(recording);
    match crate::mic::Audio16k::from_wav(&path) {
        Ok(audio) => {
            let peak = audio
                .samples()
                .iter()
                .map(|sample| sample.abs())
                .fold(0.0_f32, f32::max);
            println!(
                "wrote {}: {:.3}s, peak {:.4}",
                path.display(),
                audio.seconds(),
                peak
            );
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn meetings() -> i32 {
    use crate::meeting::{holders, Detector, Event, Kind};

    let mut detector = Detector::default();
    let mut previous = None;
    loop {
        let current = match holders::mic_holders() {
            Ok(current) => current,
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        };
        let mut holder_set = current.clone();
        holder_set.sort_by(|a, b| (a.pid, &a.bundle_id).cmp(&(b.pid, &b.bundle_id)));
        if previous.as_ref() != Some(&holder_set) {
            if current.is_empty() {
                println!("holders: none");
            } else {
                let summary = current
                    .iter()
                    .map(|holder| {
                        format!(
                            "{} {}",
                            holder.pid,
                            holder.bundle_id.as_deref().unwrap_or("-")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("holders: {summary}");
            }
            previous = Some(holder_set);
        }
        let seen = crate::meeting::classify(&current).map(|(_, app)| app);
        match detector.observe(seen, Instant::now()) {
            Some(Event::Started(app)) => {
                let kind = match app.kind {
                    Kind::Native => "native",
                    Kind::Browser => "browser",
                };
                println!("meeting started: {} ({kind})", app.name);
            }
            Some(Event::Ended(app)) => println!("meeting ended: {}", app.name),
            None => {}
        }
        std::thread::sleep(Duration::from_secs(1));
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
    let (result, inference) = match on_engine_thread(move |engine| {
        let mut utterance = crate::engine::Utterance::default();
        for block in head.chunks(crate::mic::RATE as usize / 5) {
            utterance.feed(engine, &crate::mic::Audio16k::from_samples(block.to_vec()));
        }
        let inference_start = Instant::now();
        let result = utterance.finish(engine, &tail);
        (result, inference_start.elapsed())
    }) {
        Ok(result) => result,
        Err(code) => return code,
    };
    eprintln!(
        "tail transcription: {:.3} ms",
        inference.as_secs_f64() * 1_000.0
    );
    print_text(result)
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
    let (result, inference) = match on_engine_thread(move |engine| {
        let inference_start = Instant::now();
        let result = engine.transcribe(&audio);
        (result, inference_start.elapsed())
    }) {
        Ok(result) => result,
        Err(code) => return code,
    };
    eprintln!("transcription: {:.3} ms", inference.as_secs_f64() * 1_000.0);
    eprintln!(
        "wall: {:.3} ms",
        total_start.elapsed().as_secs_f64() * 1_000.0
    );
    print_text(result)
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
            let (result, inference) = match on_engine_thread(move |engine| {
                let inference_start = Instant::now();
                let result = engine.transcribe(&audio);
                (result, inference_start.elapsed())
            }) {
                Err(code) => return code,
                Ok(result) => result,
            };
            eprintln!("transcription: {:.3} ms", inference.as_secs_f64() * 1_000.0);
            match result {
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
                Ok(transcription) => {
                    eprintln!("segments: {}", transcription.segments.len());
                    transcription
                }
            }
        }
    };
    let duration_ms = crate::clip::movie_duration_ms(&mov, &transcription.segments);
    match crate::clip::package_single_clip(
        duration_ms,
        &transcription,
        crate::clip::SessionClip {
            start_ms: 0,
            end_ms: duration_ms,
            recording_start_ms: 0,
            path: mov,
        },
    ) {
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

/// Download and load the model, then run `work` on a thread with the same
/// class the app's engine worker runs at, so the numbers this prints are the
/// numbers a dictation gets, contention included.
fn on_engine_thread<T: Send + 'static>(
    work: impl FnOnce(&mut dyn crate::engine::Engine) -> T + Send + 'static,
) -> Result<T, i32> {
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
        Ok::<_, String>(work(engine.as_mut()))
    })
    .join();
    match loaded {
        Ok(Ok(result)) => Ok(result),
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

fn print_text(result: Result<Transcription, EngineError>) -> i32 {
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
