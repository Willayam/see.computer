//! The C interface. See `include/see_core.h`.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::mpsc;

use crate::engine::{Event, Loader, Models, Phase, Worker};
use crate::mic::Audio16k;

const PROGRESS_DOWNLOADING: i32 = 0;
const PROGRESS_LOADING: i32 = 1;
const PROGRESS_WARMING: i32 = 2;
const READY: i32 = 3;
const FAILED: i32 = 4;
const PREVIEW: i32 = 5;
const DONE: i32 = 6;

type Callback = extern "C" fn(*mut c_void, i32, u64, u64, *const c_char);

pub struct SeeEngine {
    worker: Worker,
}

#[derive(Clone, Copy)]
struct Sink(Callback, *mut c_void);
unsafe impl Send for Sink {}

impl Sink {
    fn send(self, kind: i32, a: u64, b: u64, text: &str) {
        let text = CString::new(text.replace('\0', "")).unwrap_or_default();
        (self.0)(self.1, kind, a, b, text.as_ptr());
    }
}

fn owned_path(raw: *const c_char) -> PathBuf {
    PathBuf::from(
        unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// # Safety
/// Both paths are NUL-terminated. `context` stays valid for the process's life.
#[no_mangle]
pub unsafe extern "C" fn see_engine_start(
    models: *const c_char,
    dictionary: *const c_char,
    previews: bool,
    context: *mut c_void,
    callback: Callback,
) -> *mut SeeEngine {
    crate::paths::set(owned_path(models), owned_path(dictionary));
    std::env::set_var("SEE_COMPUTER_PREVIEW", if previews { "1" } else { "0" });
    let (reply, events) = mpsc::channel::<Event>();
    let worker = Worker::spawn(Loader::Models(Models::default_root()), reply);
    let sink = Sink(callback, context);
    crate::qos::spawn("see-events", crate::qos::Class::Keystroke, move || {
        for event in events {
            forward(sink, event);
        }
    });
    Box::into_raw(Box::new(SeeEngine { worker }))
}

fn forward(sink: Sink, event: Event) {
    match event {
        Event::Progress(progress) => {
            let kind = match progress.phase {
                Phase::Downloading => PROGRESS_DOWNLOADING,
                Phase::Loading => PROGRESS_LOADING,
                Phase::Warming => PROGRESS_WARMING,
            };
            sink.send(kind, progress.done, progress.total.unwrap_or(0), "");
        }
        Event::Ready(Ok(())) => sink.send(READY, 0, 0, ""),
        Event::Ready(Err(error)) => sink.send(FAILED, 0, 0, &error.to_string()),
        Event::Preview(utterance, text) => sink.send(PREVIEW, utterance, 0, &text),
        Event::Done(job, Ok(transcription)) => {
            let text = transcription.text.map(|text| text.as_str().to_owned());
            sink.send(DONE, job.0, 0, text.as_deref().unwrap_or(""));
        }
        Event::Done(job, Err(error)) => sink.send(DONE, job.0, 1, &error.to_string()),
    }
}

/// # Safety
/// `engine` came from `see_engine_start`.
#[no_mangle]
pub unsafe extern "C" fn see_engine_begin(engine: *mut SeeEngine) -> u64 {
    let (terms, receiver) = mpsc::channel();
    let _ = terms.send(Vec::new());
    (*engine).worker.begin_context(receiver)
}

fn audio(samples: *const f32, count: usize) -> Audio16k {
    if samples.is_null() || count == 0 {
        return Audio16k::from_samples(Vec::new());
    }
    Audio16k::from_samples(unsafe { std::slice::from_raw_parts(samples, count) }.to_vec())
}

/// # Safety
/// `engine` came from `see_engine_start`. `samples` holds `count` 16 kHz mono floats.
#[no_mangle]
pub unsafe extern "C" fn see_engine_feed(
    engine: *mut SeeEngine,
    samples: *const f32,
    count: usize,
) {
    (*engine).worker.feed(audio(samples, count));
}

/// # Safety
/// `engine` came from `see_engine_start`. `samples` holds `count` 16 kHz mono floats.
#[no_mangle]
pub unsafe extern "C" fn see_engine_finish(
    engine: *mut SeeEngine,
    samples: *const f32,
    count: usize,
) -> u64 {
    (*engine)
        .worker
        .submit(audio(samples, count))
        .map_or(0, |job| job.0)
}

/// # Safety
/// `engine` came from `see_engine_start`.
#[no_mangle]
pub unsafe extern "C" fn see_engine_discard(engine: *mut SeeEngine) {
    (*engine).worker.discard();
}

/// # Safety
/// `engine` came from `see_engine_start`.
#[no_mangle]
pub unsafe extern "C" fn see_engine_warm(engine: *mut SeeEngine) {
    (*engine).worker.warm();
}
