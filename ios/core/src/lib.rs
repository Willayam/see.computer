//! The Mac app's engine, compiled for iOS.
//!
//! Every `#[path]` module below is the Mac source itself, so a change to the
//! engine, the dictionary rules or the text passes reaches both apps. `seams`
//! stands in for the few Mac modules those files reach into: paths, the
//! microphone's sample type, on-screen context and the iCloud shadow copy.

// The shared files also hold what only the Mac calls.
#![allow(dead_code)]

mod seams;
use seams::{context, learn, library, mic, paths, shadow};

#[path = "../../../src-tauri/src/align.rs"]
mod align;
#[path = "../../../src-tauri/src/boost.rs"]
mod boost;
#[path = "../../../src-tauri/src/engine/mod.rs"]
mod engine;
#[path = "../../../src-tauri/src/filler.rs"]
mod filler;
#[path = "../../../src-tauri/src/numbers.rs"]
mod numbers;
#[path = "../../../src-tauri/src/qos.rs"]
mod qos;
#[path = "../../../src-tauri/src/text.rs"]
mod text;
#[path = "../../../src-tauri/src/vocabulary.rs"]
mod vocabulary;

mod ffi;
