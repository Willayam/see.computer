//! Scheduling class for every thread the app starts.
//!
//! A thread from `std::thread::spawn` runs at `QOS_CLASS_UNSPECIFIED`, which
//! the macOS scheduler ranks below anything the user is looking at. On a busy
//! machine that is exactly when dictation must not slip, so each thread names
//! the class its work belongs to instead of taking the default.
//!
//! Threads inherit the class of whoever created them, so the class is set as
//! the first statement of the thread body. ONNX Runtime's intra-op pool is
//! created later on the engine thread and inherits [`Class::Engine`] from it.

use std::thread::JoinHandle;

#[derive(Clone, Copy)]
pub enum Class {
    /// The keystroke path. Every hop between the key going down and the text
    /// landing in the user's app: the event tap, the controller, the paste.
    Keystroke,
    /// The transcription the user is waiting on. Sustained multi-core compute,
    /// which Apple asks not to run above `USER_INITIATED`.
    Engine,
    /// Upkeep nobody is waiting on. Rival polling, recorder teardown, the pill.
    Upkeep,
}

impl Class {
    fn raw(self) -> u32 {
        match self {
            Class::Keystroke => 0x21,
            Class::Engine => 0x19,
            Class::Upkeep => 0x11,
        }
    }
}

extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

pub fn apply(class: Class) {
    unsafe { pthread_set_qos_class_self_np(class.raw(), 0) };
}

pub fn spawn<T: Send + 'static>(
    name: &str,
    class: Class,
    body: impl FnOnce() -> T + Send + 'static,
) -> JoinHandle<T> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            apply(class);
            body()
        })
        .unwrap_or_else(|error| panic!("could not start the {name} thread: {error}"))
}
