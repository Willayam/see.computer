use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::engine::{EngineError, Transcription};
use crate::mic::Audio16k;

/// The microphone sink is one per process, so tests that record take turns.
static ONE_RECORDING: Mutex<()> = Mutex::new(());

/// Blocks inside its first pass until the test lets it go.
struct Blocked {
    entered: Sender<()>,
    resume: mpsc::Receiver<()>,
}

impl Engine for Blocked {
    fn transcribe(&mut self, _: &Audio16k) -> Result<Transcription, EngineError> {
        let _ = self.entered.send(());
        let _ = self.resume.recv();
        Ok(Transcription::empty())
    }
}

struct Fake {
    processes: Mutex<Result<Vec<AudioProcess>, String>>,
    /// Every tap asked for.
    taps: Mutex<Vec<String>>,
    tap_running: Arc<AtomicBool>,
    /// How long after the recording's start the tap's first sample is heard.
    tap_delay_ms: i64,
    started_ms: i64,
    engine: Mutex<Option<Held>>,
}

/// An engine the live lines get only once the test has put audio on disk.
struct Held {
    until: mpsc::Receiver<()>,
    engine: Box<dyn Engine>,
}

struct RunningTap(Arc<AtomicBool>);

impl Drop for RunningTap {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl System for Fake {
    fn processes(&self) -> Result<Vec<AudioProcess>, String> {
        self.processes.lock().unwrap().clone()
    }

    fn tap(&self, target: Target, wav: &Path) -> Result<(Box<dyn Send>, i64), TapError> {
        self.taps.lock().unwrap().push(format!("{target:?}"));
        std::fs::write(wav, b"").unwrap();
        self.tap_running.store(true, Ordering::SeqCst);
        Ok((
            Box::new(RunningTap(self.tap_running.clone())),
            self.started_ms + self.tap_delay_ms,
        ))
    }

    fn engine(&self) -> Result<Box<dyn Engine>, String> {
        let held = self
            .engine
            .lock()
            .unwrap()
            .take()
            .ok_or("no engine in this test")?;
        let _ = held.until.recv();
        Ok(held.engine)
    }
}

fn system(processes: Result<Vec<AudioProcess>, String>, started_ms: i64) -> Fake {
    Fake {
        processes: Mutex::new(processes),
        taps: Mutex::default(),
        tap_running: Arc::default(),
        tap_delay_ms: 0,
        started_ms,
        engine: Mutex::new(None),
    }
}

fn root() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "see-meeting-recorder-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn process(object: u32, bundle: &str) -> AudioProcess {
    AudioProcess {
        object,
        pid: object as i32,
        bundle_id: Some(bundle.to_owned()),
        running_input: false,
    }
}

fn zoom() -> MeetingApp {
    crate::meeting::app_for("us.zoom.xos").unwrap()
}

fn speak(seconds: usize) {
    let sink = meeting_sink().lock().unwrap();
    for _ in 0..seconds {
        sink.as_ref()
            .expect("the recording is listening")
            .send(Block {
                samples: vec![0.25; 16_000],
                rate: 16_000,
                channels: 1,
            })
            .unwrap();
    }
}

fn samples(wav: &Path) -> usize {
    hound::WavReader::open(wav).unwrap().len() as usize
}

#[test]
fn stop_cuts_off_both_inputs_while_the_live_decoder_is_still_busy() {
    let _turn = ONE_RECORDING.lock().unwrap_or_else(PoisonError::into_inner);
    let root = root();
    let (entered, in_decoder) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let (ready, wait) = mpsc::channel();
    let started_ms = now_ms();
    let system = Arc::new(Fake {
        engine: Mutex::new(Some(Held {
            until: wait,
            engine: Box::new(Blocked {
                entered,
                resume: resumed,
            }),
        })),
        ..system(Ok(Vec::new()), started_ms)
    });
    let (recording, call) = start(&root, None, started_ms, system.clone()).unwrap();
    assert_eq!(call, Call::Recorded(Scope::System));
    let folder = recording.folder.clone();

    speak(3);
    let you = folder.join(YOU);
    let on_disk = std::time::Instant::now();
    while std::fs::metadata(&you).unwrap().len() < 64_000 {
        assert!(on_disk.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    ready.send(()).unwrap();
    in_decoder
        .recv_timeout(Duration::from_secs(20))
        .expect("the live lines started a pass");

    let (done, stopped) = mpsc::channel();
    std::thread::spawn(move || done.send(recording.stop(started_ms + 5_000)));
    let stopped = stopped
        .recv_timeout(Duration::from_secs(20))
        .expect("Stop returns while the live decoder is still blocked")
        .unwrap();

    assert!(
        meeting_sink().lock().unwrap().is_none(),
        "the microphone no longer feeds the meeting"
    );
    assert!(
        !system.tap_running.load(Ordering::SeqCst),
        "the tap is gone"
    );
    assert_eq!(samples(&you), 3 * 16_000, "the track holds what was said");
    let manifest = folder::read(&folder).unwrap();
    assert_eq!(manifest.state, State::Transcribing);
    assert_eq!(manifest.ended_ms, Some(started_ms + 5_000));
    assert_eq!(stopped.lost, None);

    resume.send(()).unwrap();
    assert!(
        stopped.live.finish().is_some(),
        "the notes take over the live engine"
    );
    assert_eq!(samples(&you), 3 * 16_000);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_meeting_app_that_cannot_be_found_is_never_widened_without_the_user() {
    let _turn = ONE_RECORDING.lock().unwrap_or_else(PoisonError::into_inner);
    let root = root();
    let started_ms = now_ms();
    let system = Arc::new(Fake {
        tap_delay_ms: 90_000,
        ..system(Err("OSStatus 560947818".to_owned()), started_ms)
    });
    let (mut recording, call) = start(&root, Some(zoom()), started_ms, system.clone()).unwrap();
    assert_eq!(
        call,
        Call::Missing("macOS could not list the apps playing audio (OSStatus 560947818)".into())
    );

    *system.processes.lock().unwrap() = Ok(vec![process(9, "com.spotify.client")]);
    assert_eq!(
        recording.hear(Ask::Again),
        Call::Missing("Zoom has no audio to record yet".into())
    );
    assert!(
        system.taps.lock().unwrap().is_empty(),
        "no tap of any kind was started"
    );
    assert!(!recording.folder.join(THEM).exists());

    *system.processes.lock().unwrap() = Ok(vec![
        process(9, "com.spotify.client"),
        process(71, "us.zoom.xos"),
        process(72, "us.zoom.xos.helper"),
    ]);
    assert_eq!(recording.hear(Ask::Again), Call::Recorded(Scope::App));
    assert_eq!(*system.taps.lock().unwrap(), ["Processes([71, 72])"]);
    assert_eq!(
        folder::read(&recording.folder)
            .unwrap()
            .tracks
            .unwrap()
            .them,
        Some(CallAudio {
            scope: Scope::App,
            offset_ms: 90_000
        }),
        "live lines can place a tap that started a minute and a half in"
    );
    assert_eq!(
        recording.hear(Ask::Wider),
        Call::Recorded(Scope::App),
        "a track that is recording keeps its scope"
    );
    assert_eq!(system.taps.lock().unwrap().len(), 1);

    let folder = recording.folder.clone();
    recording.stop(started_ms + 120_000).unwrap();
    let tracks = folder::read(&folder).unwrap().tracks.unwrap();
    assert_eq!(tracks.them.unwrap().scope, Scope::App);
    assert!(tracks.you.offset_ms < 60_000);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn system_audio_is_recorded_when_the_user_picks_it_and_says_so() {
    let _turn = ONE_RECORDING.lock().unwrap_or_else(PoisonError::into_inner);
    let root = root();
    let started_ms = now_ms();
    let system = Arc::new(system(Ok(Vec::new()), started_ms));
    let (mut recording, call) = start(&root, Some(zoom()), started_ms, system.clone()).unwrap();
    assert!(matches!(call, Call::Missing(_)));
    let folder = recording.folder.clone();
    assert_eq!(folder::read(&folder).unwrap().tracks, None);

    assert_eq!(recording.hear(Ask::Wider), Call::Recorded(Scope::System));
    assert_eq!(*system.taps.lock().unwrap(), ["System"]);
    recording.stop(started_ms + 1_000).unwrap();
    assert_eq!(
        folder::read(&folder)
            .unwrap()
            .tracks
            .unwrap()
            .them
            .unwrap()
            .scope,
        Scope::System
    );

    let (alone, _) = start(&root, Some(zoom()), started_ms + 2_000, system.clone()).unwrap();
    let folder = alone.folder.clone();
    alone.stop(started_ms + 3_000).unwrap();
    assert_eq!(
        folder::read(&folder).unwrap().tracks.unwrap().them,
        None,
        "a recording left on the microphone alone says so"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn writes_stereo_blocks_as_mono_16k() {
    let wav = root().with_extension("wav");
    let (tx, rx) = std::sync::mpsc::channel();
    for _ in 0..3 {
        tx.send(Block {
            samples: vec![0.25; 48_000 * 2 / 3],
            rate: 48_000,
            channels: 2,
        })
        .unwrap();
    }
    drop(tx);
    let writer = hound::WavWriter::create(
        &wav,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    write_mic(rx, writer).unwrap();
    let mut reader = hound::WavReader::open(&wav).unwrap();
    let samples = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!((samples.len() as isize - 16_000).abs() <= 1);
    assert_eq!(samples[8_000], 8192);
    std::fs::remove_file(wav).unwrap();
}
