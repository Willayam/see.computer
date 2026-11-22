//! `see-computer transcribe <wav>`, `clip <mov>`, `meetings`, `notes <meeting-folder>`, `words [--days]`, and `tap <seconds> <out.wav> [bundle-id]` are CLI commands.
//! The first two use the same engine and model root as the app.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::engine::{EngineError, Transcription};

pub enum Cmd {
    Transcribe(PathBuf),
    TranscribeLive(PathBuf),
    TranscribePreview(PathBuf),
    Clip(PathBuf),
    Ocr(Vec<PathBuf>),
    Snapshot,
    /// The manifest, and the vocabulary to score terms against when it should
    /// differ from the one the decoder is boosting, as in a boost-off run.
    Eval(PathBuf, Option<PathBuf>),
    Meetings,
    Words {
        days: bool,
    },
    Notes(PathBuf),
    Tap(u64, PathBuf, Option<String>),
}

pub fn parse(mut args: impl Iterator<Item = String>) -> Option<Cmd> {
    match args.next().as_deref() {
        Some("transcribe") => match args.next().as_deref() {
            Some("--live") => args.next().map(PathBuf::from).map(Cmd::TranscribeLive),
            Some("--preview") => args.next().map(PathBuf::from).map(Cmd::TranscribePreview),
            Some(path) => Some(Cmd::Transcribe(PathBuf::from(path))),
            None => None,
        },
        Some("ocr") => {
            let paths: Vec<_> = args.map(PathBuf::from).collect();
            (!paths.is_empty()).then_some(Cmd::Ocr(paths))
        }
        Some("snapshot") => Some(Cmd::Snapshot),
        Some("clip") => args.next().map(PathBuf::from).map(Cmd::Clip),
        Some("eval") => args
            .next()
            .map(|manifest| Cmd::Eval(manifest.into(), args.next().map(PathBuf::from))),
        Some("words") => match args.next().as_deref() {
            None => Some(Cmd::Words { days: false }),
            Some("--days") => Some(Cmd::Words { days: true }),
            _ => None,
        },
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
        Cmd::TranscribePreview(path) => transcribe_preview(path),
        Cmd::Ocr(paths) => ocr(paths),
        Cmd::Snapshot => snapshot(),
        Cmd::Clip(path) => clip(path),
        Cmd::Eval(path, terms) => eval(path, terms),
        Cmd::Meetings => meetings(),
        Cmd::Words { days } => words(days),
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

fn ocr(paths: Vec<PathBuf>) -> i32 {
    if !crate::snapshot::ocr_enabled() {
        eprintln!("Screen OCR is disabled.");
        return 1;
    }
    let mut status = 0;
    for path in paths {
        let start = Instant::now();
        let text = crate::snapshot::ocr_file(&path);
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        match text {
            Some(text) => {
                println!("{text}");
                eprintln!("OCR {elapsed:.1} ms");
            }
            None => {
                eprintln!("OCR failed for {} after {elapsed:.1} ms", path.display());
                status = 1;
            }
        }
    }
    status
}

fn snapshot() -> i32 {
    let Some(snapshot) = crate::snapshot::Snapshot::capture_for_cli() else {
        eprintln!("Screen context is disabled.");
        return 0;
    };
    println!("{}", snapshot.render());
    let ax = &snapshot.ax;
    eprintln!("Bundle ID: {}", ax.bundle);
    eprintln!(
        "AX window lines {}, on-screen windows {}, OCR displays {}",
        ax.window.lines().count(),
        snapshot.windows.len(),
        snapshot.displays.len()
    );
    eprintln!(
        "Focus {} ms, field {} ms, AX window {} ms, window titles {} ms, OCR {}, total {} ms",
        ax.focus_ms,
        ax.field_ms,
        ax.window_ms,
        snapshot.windows_ms,
        snapshot
            .ocr_ms
            .map_or_else(|| "disabled".into(), |ms| format!("{ms} ms")),
        snapshot.total_ms
    );
    0
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
    let (result, inference) = match on_engine_thread(move |engine| {
        let inference_start = Instant::now();
        let result = replay_live(engine, &audio);
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

/// Feeds the file in the controller's 200 ms blocks and prints every preview
/// the worker would have sent, with the point in the audio it describes and
/// what it cost to decode. The worker skips one when the next block is
/// already queued; this replay never queues, so it shows the most it would run.
fn transcribe_preview(path: PathBuf) -> i32 {
    let audio = match crate::mic::Audio16k::from_wav(&path) {
        Ok(audio) => audio,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    eprintln!("audio: {:.3}s", audio.seconds());
    let result = match on_engine_thread(move |engine| {
        let block = crate::mic::RATE as usize / 5;
        let mut utterance = crate::engine::Utterance::default();
        let mut busy = Duration::ZERO;
        let mut fed = 0;
        for samples in audio.samples().chunks(block) {
            utterance.feed(
                engine,
                &crate::mic::Audio16k::from_samples(samples.to_vec()),
            );
            fed += samples.len();
            if utterance.preview_due() {
                let started = Instant::now();
                let text = utterance.preview(engine).unwrap_or_default();
                let cost = started.elapsed();
                busy += cost;
                eprintln!(
                    "{:6.1}s {:6.0} ms  {text}",
                    fed as f64 / f64::from(crate::mic::RATE),
                    cost.as_secs_f64() * 1_000.0
                );
            }
        }
        eprintln!(
            "previews: {:.0} ms of decoding over {:.1}s of audio",
            busy.as_secs_f64() * 1_000.0,
            audio.seconds()
        );
        utterance.finish(engine, &crate::mic::Audio16k::from_samples(Vec::new()))
    }) {
        Ok(result) => result,
        Err(code) => return code,
    };
    print_text(result)
}

fn replay_live(
    engine: &mut dyn crate::engine::Engine,
    audio: &crate::mic::Audio16k,
) -> Result<Transcription, EngineError> {
    let split = audio
        .samples()
        .len()
        .saturating_sub(15 * crate::mic::RATE as usize);
    let mut utterance = crate::engine::Utterance::default();
    for block in audio.samples()[..split].chunks(crate::mic::RATE as usize / 5) {
        utterance.feed(engine, &crate::mic::Audio16k::from_samples(block.to_vec()));
    }
    let tail = crate::mic::Audio16k::from_samples(audio.samples()[split..].to_vec());
    utterance.finish(engine, &tail)
}

fn eval(path: PathBuf, terms: Option<PathBuf>) -> i32 {
    let manifest = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("{}: {error}", path.display());
            return 1;
        }
    };
    let (clips, errors) = crate::eval::parse_manifest(&path, &manifest);
    let parse_skipped = errors.len();
    for error in errors {
        eprintln!("{error}");
    }
    if clips.is_empty() {
        println!(
            "clips=0 words=0 errors=0 wer=0.00 terms=0/0 insertions=0 skipped={parse_skipped}"
        );
        return if parse_skipped > 0 { 2 } else { 1 };
    }
    let lexicon =
        crate::boost::Lexicon::load(terms.unwrap_or_else(crate::boost::Lexicon::default_path));
    let terms: Vec<String> = lexicon
        .terms()
        .iter()
        .map(|term| term.text.clone())
        .collect();
    let read = |name: &str| {
        std::env::var_os(name)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default()
    };
    let context = crate::context::from_screen(
        &read("SEE_COMPUTER_EVAL_SCREEN_TEXT"),
        &crate::learn::context_terms(
            &read("SEE_COMPUTER_EVAL_DAYBOOK_TEXT"),
            crate::learn::Words::shared(),
        ),
    );
    let scored = match on_engine_thread(move |engine| {
        engine.set_context(&context);
        let mut total = crate::eval::Score::default();
        let mut count = 0;
        let mut skipped = parse_skipped;
        for clip in clips {
            if let Some(screen) = &clip.screen {
                let text = std::fs::read_to_string(screen).unwrap_or_default();
                let started = Instant::now();
                let gathered = crate::context::from_screen(&text, &[]);
                let extracted = started.elapsed();
                engine.set_context(&gathered);
                if std::env::var_os("SEE_COMPUTER_EVAL_PRINT_TERMS").is_some() {
                    let listed: Vec<&str> =
                        gathered.iter().map(|term| term.text.as_str()).collect();
                    eprintln!("clip {}: terms {}", clip.line, listed.join("|"));
                }
                eprintln!(
                    "clip {}: context {:.3} ms, boost {:.3} ms, {} terms",
                    clip.line,
                    extracted.as_secs_f64() * 1_000.0,
                    (started.elapsed() - extracted).as_secs_f64() * 1_000.0,
                    gathered.len()
                );
            }
            let audio = match crate::mic::Audio16k::from_wav(&clip.path) {
                Ok(audio) => audio,
                Err(error) => {
                    eprintln!("line {}: {error}", clip.line);
                    skipped += 1;
                    continue;
                }
            };
            let start = Instant::now();
            let hypothesis = match replay_live(engine, &audio) {
                Ok(transcription) => transcription
                    .text
                    .map(|text| text.as_str().to_owned())
                    .unwrap_or_default(),
                Err(error) => {
                    eprintln!("line {}: {error}", clip.line);
                    skipped += 1;
                    continue;
                }
            };
            let clip_terms = if clip.terms.is_empty() {
                &terms
            } else {
                &clip.terms
            };
            let score = crate::eval::score(&clip.reference, &hypothesis, clip_terms);
            println!(
                "{:.1}\t{}/{}\t{}\t{}",
                score.wer() * 100.0,
                score.hits,
                score.occurrences,
                clip.written_path,
                hypothesis
            );
            eprintln!(
                "clip {}: {:.3} ms",
                clip.line,
                start.elapsed().as_secs_f64() * 1_000.0
            );
            total.add(&score);
            count += 1;
        }
        println!(
                "clips={count} words={} errors={} wer={:.2} terms={}/{} insertions={} skipped={skipped}",
                total.words,
                total.errors,
                total.wer() * 100.0,
                total.hits,
                total.occurrences,
                total.insertions
            );
        (count, skipped)
    }) {
        Ok(result) => result,
        Err(code) => return code,
    };
    if scored.1 > 0 {
        2
    } else if scored.0 > 0 {
        0
    } else {
        1
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

fn words(days: bool) -> i32 {
    let root = std::env::var_os("SEE_COMPUTER_HISTORY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(crate::paths::documents);
    let mut tally = crate::tally::Tally::read(&root, &crate::paths::speed_log());
    if !tally.waiting.is_empty() {
        crate::library::download(&tally.waiting);
        let started = Instant::now();
        while tally
            .waiting
            .iter()
            .any(|path| crate::shadow::evicted(path))
            && started.elapsed() < Duration::from_secs(120)
        {
            std::thread::sleep(Duration::from_millis(500));
        }
        tally = crate::tally::Tally::read(&root, &crate::paths::speed_log());
    }
    if days {
        for (date, count) in &tally.days {
            println!(
                "{date}\t{}\t{}\t{}",
                count.words,
                count.dictations,
                count
                    .wpm()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".into())
            );
        }
    } else {
        let today = chrono::Local::now().date_naive();
        let total = tally.total();
        println!("words        {}", total.words);
        println!("dictations   {}", total.dictations);
        println!("today        {}", tally.on(today).words);
        println!(
            "last 7 days  {}",
            tally.since(today - chrono::Days::new(6)).words
        );
        println!(
            "speed        {}",
            total
                .wpm()
                .map(|n| format!("{n} wpm"))
                .unwrap_or_else(|| "not timed yet".into())
        );
        println!(
            "first day    {}",
            tally
                .days
                .keys()
                .next()
                .map(ToString::to_string)
                .unwrap_or_else(|| "none".into())
        );
    }
    if !tally.waiting.is_empty() {
        println!("in iCloud   {} day files not counted", tally.waiting.len());
    }
    0
}
