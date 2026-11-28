use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use super::*;
use crate::engine::{EngineError, Transcription};
use crate::meeting::folder::{CallAudio, Microphone, Scope, Tracks, MANIFEST};
use crate::meeting::live;

/// Every call the engine got, as the level of its first sample and its length.
type Calls = Arc<Mutex<Vec<(i16, usize)>>>;

/// Says the same words for the same audio, and fails where `fails` says.
struct Fake {
    calls: Calls,
    fails: Box<dyn FnMut(i16, usize) -> bool + Send>,
}

impl Fake {
    fn new(calls: &Calls) -> Fake {
        Fake {
            calls: calls.clone(),
            fails: Box::new(|_, _| false),
        }
    }

    /// `fails` gets the level of the chunk and how many calls came before.
    fn failing(calls: &Calls, fails: impl FnMut(i16, usize) -> bool + Send + 'static) -> Fake {
        Fake {
            fails: Box::new(fails),
            ..Fake::new(calls)
        }
    }
}

impl Engine for Fake {
    fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError> {
        let level = (audio.samples()[0] * 32_768.0).round() as i16;
        let before = {
            let mut calls = self.calls.lock().unwrap();
            calls.push((level, audio.samples().len()));
            calls.len() - 1
        };
        if (self.fails)(level, before) {
            return Err(EngineError::Inference("synthetic".to_owned()));
        }
        Ok(Transcription {
            text: None,
            audio: None,
            segments: vec![Segment {
                start_ms: 0,
                end_ms: (audio.samples().len() * 1_000 / RATE) as u64,
                text: format!("Level {level}."),
            }],
        })
    }
}

fn reader(engine: Fake) -> Reader {
    Reader::with(Box::new(engine))
}

/// Fails the test if the notes ask for an engine at all.
fn no_engine() -> Reader {
    Reader {
        engine: None,
        load: || Err("the engine was asked for".to_owned()),
    }
}

struct Meeting {
    root: PathBuf,
    folder: PathBuf,
}

impl Drop for Meeting {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A stopped meeting with no audio yet. `tracks` is `None` for a manifest
/// from before recordings said what they captured.
fn meeting(tracks: Option<Tracks>) -> Meeting {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "see-meeting-notes-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let folder = folder::create(&root, Some("Zoom"), 1_781_000_000_000).unwrap();
    let mut manifest = folder::read(&folder).unwrap();
    manifest.ended_ms = Some(manifest.started_ms + 600_000);
    manifest.state = State::Transcribing;
    manifest.tracks = tracks;
    folder::write(&folder, &manifest).unwrap();
    Meeting { root, folder }
}

fn both(them_offset_ms: u64) -> Option<Tracks> {
    Some(Tracks {
        you: Microphone { offset_ms: 0 },
        them: Some(CallAudio {
            scope: Scope::App,
            offset_ms: them_offset_ms,
        }),
    })
}

fn microphone_only() -> Option<Tracks> {
    Some(Tracks {
        you: Microphone { offset_ms: 0 },
        them: None,
    })
}

/// `parts` are seconds at a level, where level 0 is digital silence.
fn track(parts: &[(usize, i16)]) -> Vec<i16> {
    parts
        .iter()
        .flat_map(|&(seconds, level)| std::iter::repeat_n(level, seconds * RATE))
        .collect()
}

fn write_wav(meeting: &Meeting, speaker: Speaker, samples: &[i16]) {
    let mut writer = hound::WavWriter::create(
        meeting.folder.join(speaker.wav()),
        hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for sample in samples {
        writer.write_sample(*sample).unwrap();
    }
    writer.finalize().unwrap();
}

fn markdown(meeting: &Meeting) -> String {
    std::fs::read_to_string(meeting.folder.join(MARKDOWN)).unwrap()
}

fn lines(meeting: &Meeting) -> Vec<String> {
    markdown(meeting)
        .lines()
        .filter(|line| line.starts_with('['))
        .map(str::to_owned)
        .collect()
}

fn state(meeting: &Meeting) -> (State, Option<String>) {
    let manifest = folder::read(&meeting.folder).unwrap();
    (manifest.state, manifest.error)
}

#[test]
fn writes_both_speakers_where_they_spoke() {
    let meeting = meeting(both(90_000));
    write_wav(&meeting, Speaker::You, &track(&[(3, 0), (2, 400)]));
    write_wav(
        &meeting,
        Speaker::Them,
        &track(&[(2, 900), (4, 0), (1, 700)]),
    );
    let calls = Calls::default();
    write_notes(
        &meeting.folder,
        &mut reader(Fake::new(&calls)),
        Mode::Resume,
    )
    .unwrap();
    assert_eq!(
        lines(&meeting),
        [
            "[00:03] You: Level 400.",
            "[01:30] Them: Level 900.",
            "[01:36] Them: Level 700.",
        ],
        "each line sits where its voice starts, and the call audio sits 90 s into the meeting"
    );
}

#[test]
fn drops_the_echo_of_the_call_from_the_microphone() {
    let meeting = meeting(both(90_000));
    write_wav(
        &meeting,
        Speaker::You,
        &track(&[(1, 500), (89, 0), (1, 500)]),
    );
    write_wav(&meeting, Speaker::Them, &track(&[(1, 500)]));
    let calls = Calls::default();
    write_notes(
        &meeting.folder,
        &mut reader(Fake::new(&calls)),
        Mode::Resume,
    )
    .unwrap();
    assert_eq!(
        lines(&meeting),
        ["[00:00] You: Level 500.", "[01:30] Them: Level 500."],
        "the same words at the same moment are the speakers; the same words 90 s apart are not"
    );
}

#[test]
fn missing_audio_fails_the_meeting_by_name_before_any_engine_loads() {
    let legacy = meeting(None);
    assert_eq!(
        transcribe(&legacy.folder, &mut no_engine(), Mode::Resume).unwrap_err(),
        "you.wav is missing from the meeting folder"
    );
    assert_eq!(
        state(&legacy),
        (
            State::Failed,
            Some("you.wav is missing from the meeting folder".to_owned())
        )
    );
    assert!(!legacy.folder.join(MARKDOWN).exists());

    let neither = meeting(both(0));
    assert_eq!(
        transcribe(&neither.folder, &mut no_engine(), Mode::Resume).unwrap_err(),
        "you.wav and them.wav are missing from the meeting folder"
    );

    let one_side = meeting(both(0));
    write_wav(&one_side, Speaker::You, &track(&[(1, 300)]));
    assert_eq!(
        transcribe(&one_side.folder, &mut no_engine(), Mode::Resume).unwrap_err(),
        "them.wav is missing from the meeting folder"
    );

    write_wav(&one_side, Speaker::Them, &track(&[(1, 800)]));
    let calls = Calls::default();
    transcribe(
        &one_side.folder,
        &mut reader(Fake::new(&calls)),
        Mode::Resume,
    )
    .unwrap();
    assert_eq!(state(&one_side), (State::Done, None));
    assert_eq!(lines(&one_side).len(), 2);
}

#[test]
fn a_microphone_alone_and_real_silence_still_finish() {
    for tracks in [microphone_only(), None] {
        let alone = meeting(tracks);
        write_wav(&alone, Speaker::You, &track(&[(2, 300)]));
        let calls = Calls::default();
        transcribe(&alone.folder, &mut reader(Fake::new(&calls)), Mode::Resume).unwrap();
        assert_eq!(state(&alone), (State::Done, None));
        assert_eq!(lines(&alone), ["[00:00] You: Level 300."]);
    }

    let silent = meeting(both(0));
    write_wav(&silent, Speaker::You, &track(&[(70, 0)]));
    write_wav(&silent, Speaker::Them, &[]);
    transcribe(&silent.folder, &mut no_engine(), Mode::Resume).unwrap();
    assert_eq!(state(&silent), (State::Done, None));
    assert!(markdown(&silent).ends_with("Nothing was transcribed.\n"));
}

#[test]
fn a_track_that_is_no_recording_fails_by_name() {
    let meeting = meeting(None);
    std::fs::write(meeting.folder.join(Speaker::You.wav()), b"not audio").unwrap();
    assert_eq!(
        write_notes(&meeting.folder, &mut no_engine(), Mode::Resume).unwrap_err(),
        "you.wav is not a recording see.computer can read"
    );
}

/// Three minutes of talk with a breath every ten seconds and no digital
/// silence, like a microphone.
fn talk() -> Vec<i16> {
    let mut samples = track(&[(180, 6_000)]);
    for breath in (10 * RATE..samples.len()).step_by(10 * RATE) {
        samples[breath - 4_800..breath].fill(40);
    }
    samples
}

#[test]
fn a_failed_chunk_is_tried_again_alone_and_never_grows() {
    let meeting = meeting(microphone_only());
    write_wav(&meeting, Speaker::You, &talk());
    let calls = Calls::default();
    let first_two = Fake::failing(&calls, |_, before| before < 2);
    write_notes(&meeting.folder, &mut reader(first_two), Mode::Resume).unwrap();
    let calls = calls.lock().unwrap();
    assert!(calls.len() > 8, "{calls:?}");
    assert_eq!(calls[0], calls[1], "the retry is the same chunk");
    assert_eq!(calls[0], calls[2], "the retry is the same chunk");
    assert!(
        calls.iter().all(|&(_, length)| length <= 30 * RATE),
        "no pass ever gets more than one chunk: {calls:?}"
    );
    assert_eq!(
        calls.iter().map(|&(_, length)| length).sum::<usize>(),
        180 * RATE + 2 * calls[0].1,
        "every sample is read once, apart from the chunk that failed twice"
    );
}

/// Fifteen ten-second turns, two seconds of silence apart. Turn `n` speaks at
/// level `100 * n`.
fn turns() -> Vec<i16> {
    (1..=15)
        .flat_map(|turn| track(&[(10, 100 * turn), (2, 0)]))
        .collect()
}

#[test]
fn a_chunk_that_keeps_failing_fails_the_meeting_and_the_retry_reads_only_the_rest() {
    let meeting = meeting(microphone_only());
    write_wav(&meeting, Speaker::You, &turns());
    let calls = Calls::default();
    let third_turn = Fake::failing(&calls, |level, _| level == 300);
    assert_eq!(
        transcribe(&meeting.folder, &mut reader(third_turn), Mode::Resume).unwrap_err(),
        "you.wav could not be transcribed from 00:24 to 00:34: transcription failed: synthetic"
    );
    assert_eq!(state(&meeting).0, State::Failed);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            (100, 10 * RATE),
            (200, 10 * RATE),
            (300, 10 * RATE),
            (300, 10 * RATE),
            (300, 10 * RATE)
        ]
    );
    assert!(!meeting.folder.join(MARKDOWN).exists());

    let again = Calls::default();
    transcribe(
        &meeting.folder,
        &mut reader(Fake::new(&again)),
        Mode::Resume,
    )
    .unwrap();
    assert_eq!(state(&meeting), (State::Done, None));
    assert_eq!(
        again
            .lock()
            .unwrap()
            .iter()
            .map(|&(level, _)| level)
            .collect::<Vec<_>>(),
        (3..=15).map(|turn| 100 * turn).collect::<Vec<_>>(),
        "the two turns already read are not read again"
    );
    assert_eq!(lines(&meeting).len(), 15);
    assert!(
        !meeting.folder.join(HEARD).exists(),
        "the record goes once the notes are done"
    );
}

/// Tells the test when the first pass is under way.
struct Announced {
    inner: Box<dyn Engine>,
    entered: Option<mpsc::Sender<()>>,
}

impl Engine for Announced {
    fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError> {
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
        }
        self.inner.transcribe(audio)
    }
}

/// Runs the live lines over the tracks as they are on disk, the way they run
/// during a call, waits for the pass, and hands back the engine as Stop does.
fn live_pass(meeting: &Meeting, engine: impl Engine + 'static) -> Box<dyn Engine> {
    let (entered, started) = mpsc::channel();
    let engine = Announced {
        inner: Box::new(engine),
        entered: Some(entered),
    };
    let live = live::start(meeting.folder.clone(), move || Ok(Box::new(engine)));
    started
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the live lines read the tracks");
    live.finish().expect("the engine is handed over")
}

fn levels(calls: &Calls) -> Vec<i16> {
    let mut levels: Vec<i16> = calls.lock().unwrap().iter().map(|call| call.0).collect();
    levels.sort_unstable();
    levels
}

#[test]
fn what_the_live_lines_read_is_not_read_again() {
    let meeting = meeting(both(4_000));
    // Each track ends in a turn that live lines can only guess at, because
    // nothing after it says the turn is over.
    let you = track(&[(10, 100), (2, 0), (10, 200), (2, 0), (5, 300)]);
    let them = track(&[(3, 0), (8, 600), (2, 0), (6, 700)]);
    write_wav(&meeting, Speaker::You, &you);
    write_wav(&meeting, Speaker::Them, &them);

    let during = Calls::default();
    live_pass(&meeting, Fake::new(&during));
    assert_eq!(levels(&during), [100, 200, 300, 600, 700]);

    let after = Calls::default();
    write_notes(
        &meeting.folder,
        &mut reader(Fake::new(&after)),
        Mode::Resume,
    )
    .unwrap();
    assert_eq!(
        levels(&after),
        [300, 700],
        "only the last turn of each track was still open"
    );
    let resumed = markdown(&meeting);
    assert_eq!(
        lines(&meeting),
        [
            "[00:00] You: Level 100.",
            "[00:07] Them: Level 600.",
            "[00:12] You: Level 200.",
            "[00:17] Them: Level 700.",
            "[00:24] You: Level 300.",
        ]
    );

    let whole = Calls::default();
    write_notes(&meeting.folder, &mut reader(Fake::new(&whole)), Mode::Full).unwrap();
    assert_eq!(levels(&whole), [100, 200, 300, 600, 700]);
    assert_eq!(markdown(&meeting), resumed);

    write_notes(&meeting.folder, &mut no_engine(), Mode::Resume).unwrap();
    assert_eq!(markdown(&meeting), resumed);
}

#[test]
fn a_chunk_is_read_again_when_its_audio_changed_or_the_live_lines_failed_on_it() {
    let meeting = meeting(microphone_only());
    write_wav(
        &meeting,
        Speaker::You,
        &track(&[(10, 100), (2, 0), (10, 200), (2, 0), (10, 300), (2, 0)]),
    );
    let during = Calls::default();
    live_pass(&meeting, Fake::failing(&during, |level, _| level == 200));
    assert_eq!(levels(&during), [100, 200, 300]);

    write_wav(
        &meeting,
        Speaker::You,
        &track(&[(10, 100), (2, 0), (10, 200), (2, 0), (10, 301), (2, 0)]),
    );
    let after = Calls::default();
    write_notes(
        &meeting.folder,
        &mut reader(Fake::new(&after)),
        Mode::Resume,
    )
    .unwrap();
    assert_eq!(
        levels(&after),
        [200, 301],
        "the turn that failed live and the turn whose samples differ"
    );
    assert_eq!(
        lines(&meeting),
        [
            "[00:00] You: Level 100.",
            "[00:12] You: Level 200.",
            "[00:24] You: Level 301.",
        ]
    );
}

fn corrected(meeting: &Meeting, from: &str, to: &str) {
    let text = markdown(meeting);
    assert!(text.contains(from), "{text}");
    std::fs::write(meeting.folder.join(MARKDOWN), text.replace(from, to)).unwrap();
}

#[test]
fn reading_a_finished_meeting_again_keeps_what_the_user_corrected() {
    let meeting = meeting(microphone_only());
    write_wav(
        &meeting,
        Speaker::You,
        &track(&[(4, 100), (2, 0), (4, 200), (2, 0), (4, 300)]),
    );
    let calls = Calls::default();
    live_pass(&meeting, Fake::new(&calls));
    transcribe(
        &meeting.folder,
        &mut reader(Fake::new(&calls)),
        Mode::Resume,
    )
    .unwrap();
    assert!(
        !meeting.folder.join(PREVIOUS).exists(),
        "live lines are the app's own words, not a correction"
    );

    corrected(&meeting, "Level 100.", "Level one hundred.");
    corrected(&meeting, "Level 300.", "Level three hundred.");
    transcribe(&meeting.folder, &mut reader(Fake::new(&calls)), Mode::Full).unwrap();
    assert_eq!(
        lines(&meeting),
        [
            "[00:00] You: Level one hundred.",
            "[00:06] You: Level 200.",
            "[00:12] You: Level three hundred.",
        ]
    );
    assert!(!meeting.folder.join(PREVIOUS).exists());
    let kept = markdown(&meeting);

    // The audio of the last turn changes, so the engine reads it differently
    // and the correction made to the old reading no longer applies.
    write_wav(
        &meeting,
        Speaker::You,
        &track(&[(4, 100), (2, 0), (4, 200), (2, 0), (4, 333)]),
    );
    transcribe(&meeting.folder, &mut reader(Fake::new(&calls)), Mode::Full).unwrap();
    assert_eq!(
        lines(&meeting),
        [
            "[00:00] You: Level one hundred.",
            "[00:06] You: Level 200.",
            "[00:12] You: Level 333.",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(meeting.folder.join(PREVIOUS)).unwrap(),
        kept,
        "the corrected transcript stays in the folder"
    );
}

#[test]
fn an_older_finished_meeting_keeps_its_transcript_when_read_again() {
    let meeting = meeting(None);
    write_wav(&meeting, Speaker::You, &track(&[(2, 100)]));
    let mut manifest = folder::read(&meeting.folder).unwrap();
    manifest.state = State::Done;
    folder::write(&meeting.folder, &manifest).unwrap();
    std::fs::write(
        meeting.folder.join(MARKDOWN),
        "[00:00] You: Typed by hand.\n",
    )
    .unwrap();
    let calls = Calls::default();
    transcribe(&meeting.folder, &mut reader(Fake::new(&calls)), Mode::Full).unwrap();
    assert_eq!(lines(&meeting), ["[00:00] You: Level 100."]);
    assert_eq!(
        std::fs::read_to_string(meeting.folder.join(PREVIOUS)).unwrap(),
        "[00:00] You: Typed by hand.\n"
    );
    assert!(meeting.folder.join(MANIFEST).exists());
}

/// The speech model itself, with no vocabulary of the user's in play.
fn real_engine() -> crate::engine::parakeet::Parakeet {
    let files = crate::engine::Models::default_root()
        .ready()
        .expect("the prepared speech model, which see.computer makes on its first dictation");
    crate::engine::parakeet::Parakeet::load(&files, PathBuf::new()).unwrap()
}

struct Counted {
    inner: Box<dyn Engine>,
    calls: Arc<AtomicUsize>,
}

impl Engine for Counted {
    fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.transcribe(audio)
    }
}

fn clip(language: &str) -> Vec<i16> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures")
        .join(language)
        .with_extension("wav");
    hound::WavReader::open(path)
        .unwrap()
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A silent track of `seconds` with `voice` laid in at each of `starts`.
fn laid(seconds: usize, voice: &[i16], starts: &[usize]) -> Vec<i16> {
    let mut samples = vec![0; seconds * RATE];
    for start in starts {
        samples[start * RATE..start * RATE + voice.len()].copy_from_slice(voice);
    }
    samples
}

fn words(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphabetic())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

const ENGLISH: &str =
    "Hi, this is a quick dictation test in English. We are measuring how fast the text shows up.";
const SWEDISH: &str =
    "Hej! Det här är ett test av diktering på svenska. Vi mäter hur snabbt texten dyker upp.";

/// The audit's fixture: each speaker says the same clip twice, with silence
/// around every copy. At ffa4cfa the Swedish copies came back with 2 and 11
/// wrong words of 17, the second ending in English, while the clip alone read
/// with none.
#[test]
#[ignore = "needs the prepared speech model"]
fn real_model_reads_a_clip_in_a_meeting_as_it_reads_it_alone() {
    let meeting = meeting(both(0));
    write_wav(&meeting, Speaker::You, &laid(40, &clip("en"), &[0, 20]));
    write_wav(&meeting, Speaker::Them, &laid(40, &clip("sv"), &[8, 29]));
    transcribe(
        &meeting.folder,
        &mut Reader::with(Box::new(real_engine())),
        Mode::Resume,
    )
    .unwrap();
    let lines = lines(&meeting);
    let said: Vec<_> = lines
        .iter()
        .map(|line| line.split_once(": ").unwrap())
        .collect();
    assert_eq!(
        said.iter().map(|(who, _)| *who).collect::<Vec<_>>(),
        ["[00:00] You", "[00:08] Them", "[00:20] You", "[00:29] Them"],
        "{lines:#?}"
    );
    for ((_, text), reference) in said.iter().zip([ENGLISH, SWEDISH, ENGLISH, SWEDISH]) {
        assert_eq!(words(text), words(reference));
    }
}

/// The same fixture read live and then finished, as a call is. The second
/// Swedish copy runs to the end of its track, so the live lines can only
/// guess at it.
#[test]
#[ignore = "needs the prepared speech model"]
fn real_model_notes_after_live_lines_read_only_the_open_end_and_match_a_full_pass() {
    let meeting = meeting(both(0));
    let swedish = clip("sv");
    let mut them = laid(29, &swedish, &[8]);
    them.extend(&swedish);
    write_wav(&meeting, Speaker::You, &laid(40, &clip("en"), &[0, 20]));
    write_wav(&meeting, Speaker::Them, &them);

    let calls = Arc::new(AtomicUsize::new(0));
    let engine = live_pass(
        &meeting,
        Counted {
            inner: Box::new(real_engine()),
            calls: calls.clone(),
        },
    );
    assert_eq!(calls.swap(0, Ordering::Relaxed), 4, "one pass per clip");

    let mut reader = Reader::with(engine);
    write_notes(&meeting.folder, &mut reader, Mode::Resume).unwrap();
    assert_eq!(
        calls.swap(0, Ordering::Relaxed),
        1,
        "only the clip that was still open is read again"
    );
    let resumed = markdown(&meeting);
    for (line, reference) in lines(&meeting)
        .iter()
        .zip([ENGLISH, SWEDISH, ENGLISH, SWEDISH])
    {
        assert_eq!(words(line.split_once(": ").unwrap().1), words(reference));
    }

    write_notes(&meeting.folder, &mut reader, Mode::Full).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 4);
    assert_eq!(markdown(&meeting), resumed);
}
