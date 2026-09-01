//! Controller state machine tests.

use super::*;
use std::os::unix::fs::PermissionsExt;

fn fixture(dir: &std::path::Path, seconds: f32) -> std::path::PathBuf {
    let path = dir.join("mic.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: mic::RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec).unwrap();
    for _ in 0..(seconds * mic::RATE as f32).round() as usize {
        writer.write_sample(0_i16).unwrap();
    }
    writer.finalize().unwrap();
    path
}

fn temp_dir() -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("see-computer-test-{unique}"));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn recorder_script(dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("recorder.sh");
    std::fs::write(
            &script,
            "#!/bin/sh\nout=\nvideo=\nfor arg in \"$@\"; do\n  out=\"$arg\"\n  if [ \"$arg\" = -v ]; then video=1; fi\ndone\nif [ -z \"$video\" ]; then printf screenshot > \"$out\"; exit 0; fi\ntrap 'printf recording > \"$out\"; exit 0' INT\n: > \"$out\"\ntouch \"$out.ready\"\nwhile :; do sleep 0.02; done\n",
        )
        .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    script
}

fn test_controller_with_audio(
    seconds: f32,
) -> (Controller, Receiver<PillEvent>, std::path::PathBuf) {
    let dir = temp_dir();
    let script = recorder_script(&dir);
    let (pill_tx, pill_rx) = std::sync::mpsc::channel();
    let mut controller = Controller::for_test(Wiring {
        mic: mic::Source::Replay(fixture(&dir, seconds)),
        engine: engine::Loader::Canned("hello world".to_owned()),
        recorder: Recorder::with_program(script, dir.clone()),
        paste: paste::Paste::dry(),
        pill: pill_tx,
        trail: Trail::off(),
        history: crate::history::History::off(),
        status: std::sync::Arc::new(std::sync::Mutex::new(EngineStatus::Ready)),
        gesture: std::sync::Arc::default(),
    });
    controller.readiness = Readiness::Ready;
    (controller, pill_rx, dir)
}

fn test_controller() -> (Controller, Receiver<PillEvent>, std::path::PathBuf) {
    test_controller_with_audio(1.0)
}

#[test]
fn dictation_table_reaches_paste_and_idle() {
    let (mut controller, pill, _) = test_controller();
    controller.step(Msg::MainPressed);
    assert!(matches!(controller.session, Session::Dictating { .. }));
    controller.step(Msg::MainReleased);
    let job = match controller.session {
        Session::Transcribing { job, .. } => job,
        _ => panic!("expected transcription"),
    };
    controller.step(Msg::Engine(engine::Event::Done(
        job,
        Ok(engine::Transcription {
            text: Text::parse("hello world"),
            segments: Vec::new(),
        }),
    )));
    assert!(matches!(controller.session, Session::Pasting { .. }));
    let turn = match controller.session {
        Session::Pasting { turn, .. } => turn,
        _ => panic!("expected paste"),
    };
    controller.step(Msg::Paste(turn, paste::Outcome(Ok(paste::Landing::Pasted))));
    assert!(matches!(controller.session, Session::Idle));
    assert!(pill.try_iter().any(|event| event == PillEvent::Hide));
}

#[test]
fn shots_accumulate_without_interrupting_dictation() {
    let (mut controller, pill, _) = test_controller();
    controller.step(Msg::MainPressed);
    let since = match &controller.session {
        Session::Dictating { since, .. } => *since,
        _ => panic!("expected dictation"),
    };

    controller.step(Msg::ShotTaken(since + Duration::from_millis(400)));
    controller.step(Msg::ShotTaken(since + Duration::from_millis(700)));

    let Session::Dictating { captures, .. } = &controller.session else {
        panic!("a shot interrupted dictation");
    };
    assert_eq!(captures.len(), 2);
    let events = pill.try_iter().collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == PillEvent::Shot)
            .count(),
        2
    );
    assert!(!events.contains(&PillEvent::Show(Activity::Transcribing)));
    controller.step(Msg::Cancel);
}

#[test]
fn capture_edges_recolor_the_live_dictation_pill() {
    let (mut controller, pill, _) = test_controller();
    controller.step(Msg::MainPressed);
    let _ = pill.try_iter().collect::<Vec<_>>();

    controller.step(Msg::CaptureStarted);
    controller.step(Msg::CaptureEnded);

    assert!(matches!(controller.session, Session::Dictating { .. }));
    assert_eq!(
        pill.try_iter().collect::<Vec<_>>(),
        [
            PillEvent::Show(Activity::Recording),
            PillEvent::Show(Activity::Listening)
        ]
    );
    controller.step(Msg::Cancel);
}

#[test]
fn clip_messages_stay_inert_until_step_three() {
    let (mut controller, _, _) = test_controller();
    controller.step(Msg::MainPressed);
    let now = Instant::now();
    controller.step(Msg::ClipStarted(now));
    controller.step(Msg::ClipEnded(now + Duration::from_secs(1)));
    assert!(matches!(controller.session, Session::Dictating { .. }));
    controller.step(Msg::Cancel);
}

#[test]
fn a_grammar_clip_shorter_than_the_pipeline_minimum_degrades_to_a_shot() {
    let (mut controller, pill, _) = test_controller();
    controller.step(Msg::MainPressed);
    let since = match &controller.session {
        Session::Dictating { since, .. } => *since,
        _ => panic!("expected dictation"),
    };
    controller.step(Msg::ClipStarted(since));
    controller.step(Msg::ClipEnded(since + Duration::from_millis(400)));

    let Session::Dictating {
        captures,
        active_clip,
        ..
    } = &controller.session
    else {
        panic!("the degraded shot stopped narration");
    };
    assert!(active_clip.is_none());
    assert!(matches!(
        captures.as_slice(),
        [Capture::Shot { at_ms, .. }] if *at_ms == capture_offset_ms(since, since)
    ));
    assert!(pill.try_iter().any(|event| event == PillEvent::Shot));
    controller.step(Msg::Cancel);
}

#[test]
fn a_clip_uses_the_finger_release_instant_as_its_end() {
    let (mut controller, _, _) = test_controller();
    controller.step(Msg::MainPressed);
    let since = match &controller.session {
        Session::Dictating { since, .. } => *since,
        _ => panic!("expected dictation"),
    };
    let pressed = since + Duration::from_millis(100);
    let released = pressed + Duration::from_millis(900);
    controller.step(Msg::ClipStarted(pressed));
    controller.step(Msg::ClipEnded(released));

    let Session::Dictating { captures, .. } = &controller.session else {
        panic!("ending the clip stopped narration");
    };
    assert!(matches!(
        captures.as_slice(),
        [Capture::Clip {
            start_ms,
            end_ms,
            ..
        }] if *start_ms == capture_offset_ms(since, pressed)
            && *end_ms == capture_offset_ms(since, released)
            && end_ms - start_ms == 900
    ));
    controller.step(Msg::Cancel);
}

#[test]
fn a_silent_shot_session_still_writes_and_pastes_its_folder() {
    let (mut controller, _, _) = test_controller_with_audio(0.4);
    controller.step(Msg::MainPressed);
    let since = match &controller.session {
        Session::Dictating { since, .. } => *since,
        _ => panic!("expected dictation"),
    };
    controller.step(Msg::ShotTaken(since + Duration::from_millis(50)));
    let session_dir = match &controller.session {
        Session::Dictating {
            capture_dir: Some(dir),
            ..
        } => dir.clone(),
        _ => panic!("expected a captured shot"),
    };
    controller.step(Msg::MainReleased);
    let job = match controller.session {
        Session::TranscribingTake { job, .. } => job,
        _ => panic!("a captured session must survive the short-audio guard"),
    };
    controller.step(Msg::Engine(engine::Event::Done(
        job,
        Ok(engine::Transcription::empty()),
    )));

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "shot package never finished");
        let Ok(message) = controller.rx.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        if let Msg::Packaged(turn, result) = message {
            let paste = result.unwrap();
            assert_eq!(
                paste,
                format!(
                    "No narration.\n\n1 screenshot: {}",
                    session_dir.join("take.md").display()
                )
            );
            controller.step(Msg::Packaged(turn, Ok(paste)));
            break;
        }
        controller.step(message);
    }

    assert!(matches!(controller.session, Session::Pasting { .. }));
    assert!(session_dir.join("shots/001.png").is_file());
    assert!(session_dir.join("transcript.json").is_file());
    assert!(session_dir.join("take.md").is_file());
    let transcript: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(session_dir.join("transcript.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(transcript["captures"][0]["atMs"], 350);
    assert!(!std::fs::read_dir(session_dir)
        .unwrap()
        .flatten()
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "mov")));
}

#[test]
fn zero_capture_release_uses_the_unchanged_dictation_path() {
    let (mut controller, _, dir) = test_controller();
    controller.step(Msg::MainPressed);
    controller.step(Msg::MainReleased);
    let job = match controller.session {
        Session::Transcribing { job, .. } => job,
        _ => panic!("zero captures must use plain transcription"),
    };
    controller.step(Msg::Engine(engine::Event::Done(
        job,
        Ok(engine::Transcription {
            text: Text::parse("hello world"),
            segments: Vec::new(),
        }),
    )));
    assert_eq!(
        controller.paste.last_text().as_deref(),
        Some("hello world ")
    );
    assert!(matches!(controller.session, Session::Pasting { .. }));
    assert!(!std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .any(|entry| entry.path().is_dir()));
}

#[test]
fn shot_to_sentence_offset_includes_mic_preroll() {
    let dir = temp_dir();
    let session_dir = dir.join("session");
    let shot_path = session_dir.join("shots/001.png");
    std::fs::create_dir_all(shot_path.parent().unwrap()).unwrap();
    std::fs::write(&shot_path, b"screenshot").unwrap();
    let since = Instant::now();
    let at_ms = capture_offset_ms(since, since + Duration::from_millis(500));
    let preroll_ms = u64::try_from(mic::PREROLL.as_millis()).unwrap();
    assert_eq!(at_ms, 500 + preroll_ms);
    assert_eq!(
        capture_offset_ms(since, since - Duration::from_millis(100)),
        preroll_ms - 100
    );

    let transcription = engine::Transcription {
        text: Text::parse("First sentence. Second sentence."),
        segments: vec![
            engine::Segment {
                start_ms: 0,
                end_ms: 700,
                text: "First sentence.".to_owned(),
            },
            engine::Segment {
                start_ms: 700,
                end_ms: 1_200,
                text: "Second sentence.".to_owned(),
            },
        ],
    };
    let packaged = clip::package_session(
        &session_dir,
        1_200,
        &transcription,
        &[clip::SessionCapture::Shot(clip::Shot {
            at_ms,
            path: shot_path,
        })],
    )
    .unwrap();
    let markdown = std::fs::read_to_string(packaged.markdown).unwrap();
    let first = markdown.find("First sentence.").unwrap();
    let second = markdown.find("Second sentence.").unwrap();
    let shot = markdown.find("shots/001.png").unwrap();
    assert!(
        first < second && second < shot,
        "the shot belongs to the second sentence"
    );
    let json = std::fs::read_to_string(session_dir.join("transcript.json")).unwrap();
    assert!(json.contains("\"atMs\": 800"));
}

#[test]
fn ignored_and_notice_cells_are_table_driven() {
    type TableCase = (&'static str, fn(&mut Controller), fn() -> Msg, &'static str);
    let cases: &[TableCase] = &[
        ("idle release", |_| {}, || Msg::MainReleased, "Idle"),
        ("idle cancel", |_| {}, || Msg::Cancel, "Idle"),
        (
            "dictating repeat",
            |controller| controller.step(Msg::MainPressed),
            || Msg::MainPressed,
            "Dictating",
        ),
    ];
    for (name, prepare, message, expected) in cases {
        let (mut controller, _, _) = test_controller();
        prepare(&mut controller);
        controller.step(message());
        assert_eq!(session_label(&controller.session), *expected, "{name}");
    }
}

#[test]
fn stale_job_is_ignored_and_cancel_drains_dictation() {
    let (mut controller, _, _) = test_controller();
    controller.step(Msg::MainPressed);
    controller.step(Msg::MainReleased);
    let stale = controller
        .engine
        .submit(crate::mic::Audio16k::silence(0.3))
        .unwrap();
    controller.step(Msg::Engine(engine::Event::Done(
        stale,
        Ok(engine::Transcription::empty()),
    )));
    assert!(matches!(controller.session, Session::Transcribing { .. }));

    let (mut controller, _, _) = test_controller();
    controller.step(Msg::MainPressed);
    controller.step(Msg::Cancel);
    assert!(matches!(controller.session, Session::Idle));
}

#[test]
fn tap_guard_finishes_nothing_heard() {
    let (mut controller, pill, _) = test_controller_with_audio(0.4);
    controller.step(Msg::MainPressed);
    controller.step(Msg::MainReleased);
    assert!(matches!(controller.session, Session::Idle));
    assert!(pill
        .try_iter()
        .any(|event| event == PillEvent::Finish(Notice::NothingHeard)));
}

#[test]
fn stale_stream_with_empty_audio_reports_mic_unavailable() {
    let notice = incomplete_dictation_notice(0, &mic::Audio16k::from_samples(Vec::new()), false);
    assert_eq!(
        notice,
        Some(Notice::MicUnavailable("audio stream stopped".to_owned()))
    );
}

#[test]
fn timeout_returns_to_idle() {
    let (mut controller, _, _) = test_controller();
    controller.step(Msg::MainPressed);
    controller.step(Msg::MainReleased);
    if let Session::Transcribing { since, .. } = &mut controller.session {
        *since = Instant::now() - TRANSCRIBE_TIMEOUT;
    }
    controller.expire();
    assert!(matches!(controller.session, Session::Idle));
}

#[test]
fn trail_uses_stable_labels() {
    let path = temp_dir().join("trail.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    let mut trail = Trail(Some(file));
    trail.record("Idle", "MainPressed", "Dictating");
    let line = std::fs::read_to_string(path).unwrap();
    assert!(line.ends_with("\tIdle\tMainPressed\tDictating\n"));
}
