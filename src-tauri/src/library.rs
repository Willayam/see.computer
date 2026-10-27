use std::path::Path;
use std::sync::OnceLock;

use chrono::{Local, NaiveDateTime, TimeZone};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::history::History;
use crate::ledger::Ledger;
use crate::meeting::folder;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    id: String,
    kind: &'static str,
    state: &'static str,
    error: Option<String>,
    at: i64,
    duration_ms: Option<u64>,
    title: String,
    source: Option<String>,
    lines: Vec<Line>,
    media: Vec<Media>,
    paste: String,
    files: Files,
    share: Option<Share>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    start_ms: i64,
    end_ms: Option<i64>,
    speaker: Option<&'static str>,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Media {
    kind: &'static str,
    at_ms: i64,
    duration_ms: Option<u64>,
    speaker: Option<&'static str>,
    path: String,
    poster: Option<String>,
}

#[derive(Serialize)]
struct Files {
    folder: String,
    markdown: Option<String>,
}

#[derive(Serialize)]
struct Share {
    url: String,
}

static APP: OnceLock<AppHandle> = OnceLock::new();

extern "C" {
    fn see_library_close_on_command_q();
}

pub fn attach(app: &AppHandle) {
    let _ = APP.set(app.clone());
    unsafe { see_library_close_on_command_q() };
}

pub fn changed() {
    if let Some(app) = APP.get() {
        if app.get_webview_window("library").is_some() {
            let _ = app.emit_to("library", "library-changed", ());
        }
    }
}

pub fn show(app: &AppHandle, select: Option<String>) {
    if let Some(window) = app.get_webview_window("library") {
        let _ = window.show();
        let _ = window.set_focus();
        if let Some(id) = select {
            let _ = app.emit_to("library", "library-select", id);
        }
        activate(app);
        return;
    }
    let url = select.map_or_else(
        || "library.html".to_owned(),
        |id| format!("library.html#{id}"),
    );
    match WebviewWindowBuilder::new(app, "library", WebviewUrl::App(url.into()))
        .title("Library")
        .inner_size(1040.0, 700.0)
        .min_inner_size(720.0, 480.0)
        .resizable(true)
        .center()
        .focused(true)
        .build()
    {
        Ok(window) => {
            let closing = app.clone();
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    // The next dictation pastes into the frontmost app, so
                    // that has to be the one the user came from, not us.
                    set_active(&closing, false);
                }
            });
            activate(app);
        }
        Err(error) => eprintln!("could not open Library: {error}"),
    }
}

/// The app the user was in when the Library came forward, by process id.
/// `[NSApp deactivate]` alone leaves see.computer as the workspace's
/// frontmost app after the window closes, and the next dictation would then
/// paste into nothing, so closing hands activation back to this app.
static CAME_FROM: std::sync::Mutex<Option<i32>> = std::sync::Mutex::new(None);

fn activate(app: &AppHandle) {
    set_active(app, true);
}

fn set_active(app: &AppHandle, active: bool) {
    let _ = app.run_on_main_thread(move || unsafe {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};
        let (Some(application), Some(workspace), Some(running)) = (
            AnyClass::get(c"NSApplication"),
            AnyClass::get(c"NSWorkspace"),
            AnyClass::get(c"NSRunningApplication"),
        ) else {
            return;
        };
        let application: *mut AnyObject = msg_send![application, sharedApplication];
        let ours = std::process::id() as i32;
        let mut came_from = CAME_FROM
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active {
            let shared: *mut AnyObject = msg_send![workspace, sharedWorkspace];
            let front: *mut AnyObject = msg_send![shared, frontmostApplication];
            if !front.is_null() {
                let pid: i32 = msg_send![front, processIdentifier];
                if pid != ours {
                    *came_from = Some(pid);
                }
            }
            let _: () = msg_send![application, activate];
            return;
        }
        let previous: *mut AnyObject = match came_from.take() {
            Some(pid) => msg_send![running, runningApplicationWithProcessIdentifier: pid],
            None => std::ptr::null_mut(),
        };
        if previous.is_null() {
            let _: () = msg_send![application, deactivate];
            return;
        }
        let current: *mut AnyObject = msg_send![running, currentApplication];
        let _: bool = msg_send![previous, activateFromApplication: current, options: 0usize];
    });
}

/// Async so the file reads run off the main thread, which also draws the panel.
#[tauri::command]
pub async fn library_items(history: State<'_, History>) -> Result<Vec<Item>, String> {
    let root = history.root();
    fetch(&root, std::time::Duration::from_secs(3));
    Ok(items(&root))
}

/// When the disk fills, macOS moves files in an iCloud Documents folder to
/// the cloud and leaves a placeholder. Reading one waits for its download,
/// 1.2 to 1.4 s each, and the downloads run one after another, so a Library
/// that read every file was blank for minutes. Asking for all of them at
/// once brought 30 back in 2.5 s on 2026-10-16.
const SF_DATALESS: u32 = 0x4000_0000;

pub(crate) fn evicted(path: &Path) -> bool {
    use std::os::darwin::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.st_flags() & SF_DATALESS != 0)
}

/// The files `items` reads, as opposed to the media it only links to.
fn read_by_items(history_root: &Path, documents: &Path) -> Vec<std::path::PathBuf> {
    let listed = |dir: &Path| {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>()
    };
    let mut files: Vec<_> = listed(history_root)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .collect();
    for dir in listed(documents).into_iter().filter(|path| path.is_dir()) {
        for name in [
            folder::MANIFEST,
            folder::MARKDOWN,
            "transcript.json",
            "share.json",
        ] {
            files.push(dir.join(name));
        }
    }
    files
}

/// How many files the Library reads are still in iCloud.
pub(crate) fn in_icloud(history_root: &Path) -> usize {
    read_by_items(history_root, &crate::paths::documents())
        .iter()
        .filter(|path| evicted(path))
        .count()
}

static FETCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Asks iCloud for every evicted file the Library reads and waits up to
/// `wait` for them. Any still in the cloud show as downloading, and the
/// Library reloads once they arrive. While that watch runs, a load does not
/// wait again, so a stalled download costs one wait, not one per load.
pub(crate) fn fetch(history_root: &Path, wait: std::time::Duration) {
    use std::sync::atomic::Ordering;
    if FETCHING.load(Ordering::SeqCst) {
        return;
    }
    let documents = crate::paths::documents();
    let pending = |files: &[std::path::PathBuf]| -> Vec<std::path::PathBuf> {
        files.iter().filter(|path| evicted(path)).cloned().collect()
    };
    let missing = pending(&read_by_items(history_root, &documents));
    if missing.is_empty() {
        return;
    }
    download(&missing);
    let deadline = std::time::Instant::now() + wait;
    let mut left = missing;
    while !left.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
        left = pending(&left);
    }
    if left.is_empty() || FETCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        let mut reported = std::time::Instant::now();
        let mut before = left.len();
        while !left.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(500));
            left = pending(&left);
            if left.len() < before && (left.is_empty() || reported.elapsed().as_secs() >= 5) {
                changed();
                reported = std::time::Instant::now();
                before = left.len();
            }
        }
        FETCHING.store(false, Ordering::SeqCst);
    });
}

pub(crate) fn download(paths: &[std::path::PathBuf]) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    objc2::rc::autoreleasepool(|_| unsafe {
        let (Some(manager), Some(url), Some(string)) = (
            AnyClass::get(c"NSFileManager"),
            AnyClass::get(c"NSURL"),
            AnyClass::get(c"NSString"),
        ) else {
            return;
        };
        let manager: *mut AnyObject = msg_send![manager, defaultManager];
        for path in paths {
            let Ok(path) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else {
                continue;
            };
            let path: *mut AnyObject = msg_send![string, stringWithUTF8String: path.as_ptr()];
            if path.is_null() {
                continue;
            }
            let file: *mut AnyObject = msg_send![url, fileURLWithPath: path];
            let error: *mut *mut AnyObject = std::ptr::null_mut();
            let _: bool =
                msg_send![manager, startDownloadingUbiquitousItemAtURL: file, error: error];
        }
    });
}

/// What a file still in iCloud shows as until it downloads.
fn in_cloud(
    id: String,
    kind: &'static str,
    at: i64,
    folder: &Path,
    markdown: Option<String>,
) -> Item {
    Item {
        id,
        kind,
        state: "cloud",
        error: None,
        at,
        duration_ms: None,
        title: "Downloading from iCloud".to_owned(),
        source: None,
        lines: Vec::new(),
        media: Vec::new(),
        paste: String::new(),
        files: Files {
            folder: path_string(folder),
            markdown,
        },
        share: None,
    }
}

#[tauri::command]
pub fn library_copy(text: String) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn library_reveal(path: String, history: State<'_, History>) -> Result<(), String> {
    let target = Path::new(&path)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let allowed = [crate::paths::documents(), history.root()];
    if !allowed.iter().any(|root| {
        root.canonicalize()
            .is_ok_and(|root| target.starts_with(root))
    }) {
        return Err("path is outside the Library".to_owned());
    }
    std::process::Command::new("open")
        .arg("-R")
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn library_retry(
    folder: String,
    meetings: State<'_, crate::meeting::controller::Meetings>,
) -> Result<(), String> {
    let folder = Path::new(&folder)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !crate::paths::documents()
        .canonicalize()
        .is_ok_and(|root| folder.starts_with(root))
    {
        return Err("path is outside the Library".to_owned());
    }
    meetings.retry(&folder)
}

/// Fixes one transcript line where the Library shows it, then teaches the
/// learner from the fix, as the Fix dialog did. Dictations are rewritten by
/// the history thread, which owns the day files; a finished meeting's
/// markdown has no other writer.
#[tauri::command]
pub async fn library_fix(
    id: String,
    start_ms: i64,
    original: String,
    edited: String,
    history: State<'_, History>,
    ledger: State<'_, Ledger>,
) -> Result<(), String> {
    let edited = edited.trim();
    if edited.is_empty() {
        return Err("Type the corrected text, or press Cancel.".to_owned());
    }
    if edited == original.trim() {
        return Ok(());
    }
    match id.split_once(':') {
        Some(("dictation", file)) => {
            if edited.lines().any(|line| line.starts_with("## ")) {
                return Err(
                    "Start no line with \"## \"; the history uses it to mark a dictation."
                        .to_owned(),
                );
            }
            let at = Local
                .timestamp_millis_opt(start_ms)
                .single()
                .ok_or("That dictation has no valid time.")?
                .naive_local();
            history
                .replace(crate::history::Fix {
                    file: file.to_owned(),
                    at,
                    original: original.clone(),
                    edited: edited.to_owned(),
                })
                .recv()
                .map_err(|_| "Unable to save. Quit and reopen see.computer, then try again.")??;
            ledger.fixed(start_ms, &original, edited);
            Ok(())
        }
        Some(("meeting", name)) if !name.contains(['/', '\\']) && name != ".." => {
            if edited.contains('\n') {
                return Err("Keep a meeting line on one line.".to_owned());
            }
            let folder = crate::paths::documents().join(name);
            let manifest =
                folder::read(&folder).ok_or("That meeting is no longer in the Library.")?;
            if manifest.state != folder::State::Done {
                return Err("Wait until the meeting has finished transcribing.".to_owned());
            }
            let path = folder.join(folder::MARKDOWN);
            let failed = |error: std::io::Error| format!("Unable to save: {error}.");
            let before = std::fs::read_to_string(&path).map_err(failed)?;
            let after = fixed_meeting(&before, start_ms, &original, edited)
                .ok_or("That line changed on disk, so the Library has been reloaded.")?;
            let temp = path.with_extension("md.tmp");
            std::fs::write(&temp, after).map_err(failed)?;
            if std::fs::read_to_string(&path).ok().as_deref() != Some(before.as_str()) {
                let _ = std::fs::remove_file(&temp);
                return Err("The meeting changed while saving. Try again.".to_owned());
            }
            std::fs::rename(temp, &path).map_err(failed)?;
            changed();
            ledger.fixed(manifest.started_ms + start_ms, &original, edited);
            Ok(())
        }
        _ => Err("Only dictations and meetings can be fixed here.".to_owned()),
    }
}

/// One spoken line from a dictation or a meeting, for the Dictionary's
/// counts. Takes are left out: their words are in the day files already.
pub struct Spoken {
    pub item: String,
    pub start_ms: i64,
    pub at_ms: i64,
    pub text: String,
}

/// Every dictation and meeting line, newest first.
pub fn spoken(history_root: &Path) -> Vec<Spoken> {
    let mut out: Vec<Spoken> = items(history_root)
        .into_iter()
        .filter(|item| matches!(item.kind, "dictation" | "meeting"))
        .flat_map(|item| {
            let base = if item.kind == "meeting" { item.at } else { 0 };
            item.lines.into_iter().map(move |line| Spoken {
                item: item.id.clone(),
                start_ms: line.start_ms,
                at_ms: base + line.start_ms,
                text: line.text,
            })
        })
        .collect();
    out.sort_by_key(|line| std::cmp::Reverse(line.at_ms));
    out
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn local_ms(date: &str, time: &str) -> Option<i64> {
    let naive =
        NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S").ok()?;
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|at| at.timestamp_millis())
}

fn day(path: &Path) -> Option<Item> {
    let file = path.file_stem()?.to_str()?;
    let date = crate::history::day_file_date(file)?
        .format("%Y-%m-%d")
        .to_string();
    if evicted(path) {
        return Some(in_cloud(
            format!("dictation:{file}"),
            "dictation",
            local_ms(&date, "00:00:00")?,
            path.parent()?,
            Some(path_string(path)),
        ));
    }
    let content = std::fs::read_to_string(path).ok()?;
    let mut lines = Vec::new();
    let mut time = None;
    let mut body = String::new();
    for line in content.lines() {
        if let Some(next) = line
            .strip_prefix("## ")
            .and_then(|value| local_ms(&date, value))
        {
            if let Some(at) = time.take() {
                lines.push(Line {
                    start_ms: at,
                    end_ms: None,
                    speaker: None,
                    text: body.trim().to_owned(),
                });
            }
            time = Some(next);
            body.clear();
        } else if time.is_some() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(line);
        }
    }
    if let Some(at) = time {
        lines.push(Line {
            start_ms: at,
            end_ms: None,
            speaker: None,
            text: body.trim().to_owned(),
        });
    }
    let at = lines.first()?.start_ms;
    let paste = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(Item {
        id: format!("dictation:{file}"),
        kind: "dictation",
        state: "done",
        error: None,
        at,
        duration_ms: None,
        title: format!(
            "{} dictation{}",
            lines.len(),
            if lines.len() == 1 { "" } else { "s" }
        ),
        source: None,
        lines,
        media: Vec::new(),
        paste,
        files: Files {
            folder: path_string(path.parent()?),
            markdown: Some(path_string(path)),
        },
        share: None,
    })
}

/// `[mm:ss] You: text` as its start, speaker, and where the text begins.
fn meeting_line(line: &str) -> Option<(i64, &'static str, usize)> {
    let (clock, rest) = line.strip_prefix('[')?.split_once("] ")?;
    let parts = clock
        .split(':')
        .map(str::parse::<i64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let seconds = match parts.as_slice() {
        [minutes, seconds] => minutes * 60 + seconds,
        [hours, minutes, seconds] => hours * 3600 + minutes * 60 + seconds,
        _ => return None,
    };
    let (speaker, _) = rest.split_once(": ")?;
    let speaker = match speaker {
        "You" => "you",
        "Them" => "them",
        _ => return None,
    };
    Some((seconds * 1000, speaker, clock.len() + 3 + speaker.len() + 2))
}

fn meeting_lines(markdown: &str) -> Vec<Line> {
    markdown
        .lines()
        .filter_map(|line| {
            let (start_ms, speaker, text) = meeting_line(line)?;
            Some(Line {
                start_ms,
                end_ms: None,
                speaker: Some(speaker),
                text: line[text..].trim().to_owned(),
            })
        })
        .collect()
}

/// The markdown with one line's words replaced, or `None` when no line at
/// that time still reads as `original`. Every other byte is kept.
fn fixed_meeting(markdown: &str, start_ms: i64, original: &str, edited: &str) -> Option<String> {
    let mut out = String::with_capacity(markdown.len() + edited.len());
    let mut done = false;
    for chunk in markdown.split_inclusive('\n') {
        let body = chunk.trim_end_matches(['\n', '\r']);
        match meeting_line(body) {
            Some((at, _, text))
                if !done && at == start_ms && body[text..].trim() == original.trim() =>
            {
                out.push_str(&body[..text]);
                out.push_str(edited.trim());
                out.push_str(&chunk[body.len()..]);
                done = true;
            }
            _ => out.push_str(chunk),
        }
    }
    done.then_some(out)
}

fn meeting(path: &Path) -> Option<Item> {
    let name = path.file_name()?.to_str()?;
    if evicted(&path.join(folder::MANIFEST)) || evicted(&path.join(folder::MARKDOWN)) {
        let started =
            NaiveDateTime::parse_from_str(name.strip_prefix("meeting-")?, "%Y-%m-%d-%H-%M-%S")
                .ok()?;
        let at = Local
            .from_local_datetime(&started)
            .single()?
            .timestamp_millis();
        return Some(in_cloud(
            format!("meeting:{name}"),
            "meeting",
            at,
            path,
            None,
        ));
    }
    let manifest = folder::read(path)?;
    let markdown = path.join(folder::MARKDOWN);
    let present = markdown.is_file();
    let paste = std::fs::read_to_string(&markdown).unwrap_or_default();
    let media = [(folder::YOU, "you"), (folder::THEM, "them")]
        .into_iter()
        .filter_map(|(name, speaker)| {
            let file = path.join(name);
            file.is_file().then(|| Media {
                kind: "audio",
                at_ms: 0,
                duration_ms: None,
                speaker: Some(speaker),
                path: path_string(&file),
                poster: None,
            })
        })
        .collect();
    Some(Item {
        id: format!("meeting:{}", path.file_name()?.to_string_lossy()),
        kind: "meeting",
        state: match manifest.state {
            folder::State::Done => "done",
            folder::State::Recording => "recording",
            folder::State::Transcribing => "transcribing",
            folder::State::Failed => "failed",
        },
        error: manifest.error.clone(),
        at: manifest.started_ms,
        duration_ms: manifest.duration_ms(),
        title: folder::title(path, &manifest),
        source: manifest.app,
        lines: meeting_lines(&paste),
        media,
        paste,
        files: Files {
            folder: path_string(path),
            markdown: present.then(|| path_string(&markdown)),
        },
        share: None,
    })
}

fn take(path: &Path) -> Option<Item> {
    let markdown = [path.join("take.md"), path.join("clip.md")]
        .into_iter()
        .find(|file| file.is_file());
    if evicted(&path.join("transcript.json")) {
        let name = path.file_name()?.to_str()?;
        let at = name
            .get(..10)
            .zip(name.get(11..))
            .and_then(|(date, clock)| local_ms(date, &clock.replace('-', ":")))?;
        let markdown = markdown.as_ref().map(|file| path_string(file));
        return Some(in_cloud(format!("take:{name}"), "take", at, path, markdown));
    }
    let transcript = std::fs::read(path.join("transcript.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_default();
    if markdown.is_none() && transcript.is_null() {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let at = name
        .get(..10)
        .zip(name.get(11..))
        .and_then(|(date, clock)| local_ms(date, &clock.replace('-', ":")))
        .or_else(|| {
            std::fs::metadata(path)
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_millis() as i64)
        })?;
    let full_text = transcript
        .get("fullText")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let lines = transcript
        .get("segments")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|segment| {
            Some(Line {
                start_ms: segment.get("startMs")?.as_i64()?,
                end_ms: segment.get("endMs").and_then(serde_json::Value::as_i64),
                speaker: None,
                text: segment.get("text")?.as_str()?.to_owned(),
            })
        })
        .collect();
    let mut media = Vec::new();
    for capture in transcript
        .get("captures")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        match capture.get("type").and_then(serde_json::Value::as_str) {
            Some("shot") => {
                if let (Some(file), Some(at_ms)) = (
                    capture.get("file").and_then(serde_json::Value::as_str),
                    capture.get("atMs").and_then(serde_json::Value::as_i64),
                ) {
                    media.push(Media {
                        kind: "shot",
                        at_ms,
                        duration_ms: None,
                        speaker: None,
                        path: path_string(&path.join(file)),
                        poster: None,
                    });
                }
            }
            Some("clip") => {
                if let (Some(file), Some(at_ms)) = (
                    capture.get("video").and_then(serde_json::Value::as_str),
                    capture.get("startMs").and_then(serde_json::Value::as_i64),
                ) {
                    let poster = capture
                        .get("frames")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|frames| frames.first())
                        .and_then(|frame| frame.get("file"))
                        .and_then(serde_json::Value::as_str)
                        .map(|file| path_string(&path.join(file)));
                    media.push(Media {
                        kind: "clip",
                        at_ms,
                        duration_ms: capture
                            .get("durationMs")
                            .and_then(serde_json::Value::as_u64),
                        speaker: None,
                        path: path_string(&path.join(file)),
                        poster,
                    });
                }
                for frame in capture
                    .get("frames")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let (Some(file), Some(at_ms)) = (
                        frame.get("file").and_then(serde_json::Value::as_str),
                        frame.get("atMs").and_then(serde_json::Value::as_i64),
                    ) {
                        media.push(Media {
                            kind: "frame",
                            at_ms,
                            duration_ms: None,
                            speaker: None,
                            path: path_string(&path.join(file)),
                            poster: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    media.sort_by_key(|entry| entry.at_ms);
    let markdown_path = markdown.as_ref().map(|path| path_string(path));
    let share = (!evicted(&path.join("share.json")))
        .then(|| std::fs::read(path.join("share.json")).ok())
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(|url| Share {
                    url: url.to_owned(),
                })
        });
    let title = full_text
        .split_whitespace()
        .take(7)
        .collect::<Vec<_>>()
        .join(" ");
    let fallback = path_string(path);
    Some(Item {
        id: format!("take:{name}"),
        kind: "take",
        state: "done",
        error: None,
        at,
        duration_ms: transcript
            .get("durationMs")
            .and_then(serde_json::Value::as_u64),
        title: if title.is_empty() {
            "Screen take".to_owned()
        } else {
            title
        },
        source: None,
        lines,
        media,
        paste: format!(
            "{full_text}\n\n{}",
            markdown_path.as_deref().unwrap_or(&fallback)
        ),
        files: Files {
            folder: path_string(path),
            markdown: markdown_path,
        },
        share,
    })
}

fn bare_movie(path: &Path) -> Option<Item> {
    let name = path.file_name()?.to_str()?;
    let at = std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    let paste = url::Url::from_file_path(path).ok()?.to_string();
    Some(Item {
        id: format!("take:{name}"),
        kind: "take",
        state: "done",
        error: None,
        at,
        duration_ms: None,
        title: "Screen take".to_owned(),
        source: None,
        lines: Vec::new(),
        media: vec![Media {
            kind: "clip",
            at_ms: 0,
            duration_ms: None,
            speaker: None,
            path: path_string(path),
            poster: None,
        }],
        paste,
        files: Files {
            folder: path_string(path.parent()?),
            markdown: None,
        },
        share: None,
    })
}

fn items(history_root: &Path) -> Vec<Item> {
    items_in(history_root, &crate::paths::documents())
}

fn items_in(history_root: &Path, documents: &Path) -> Vec<Item> {
    let mut result = Vec::new();
    for path in std::fs::read_dir(history_root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
    {
        if path.extension().is_some_and(|extension| extension == "md") {
            if let Some(item) = day(&path) {
                result.push(item);
            }
        }
    }
    for path in std::fs::read_dir(documents)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
    {
        if path.is_dir() {
            if let Some(item) = meeting(&path).or_else(|| take(&path)) {
                result.push(item);
            }
        } else if path.extension().is_some_and(|extension| extension == "mov")
            && !path.with_extension("").is_dir()
        {
            if let Some(item) = bare_movie(&path) {
                result.push(item);
            }
        }
    }
    result.sort_by_key(|item| std::cmp::Reverse(item.at));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "see-library-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn dictation_day_has_first_epoch_and_paste() {
        let root = temp_dir();
        let path = root.join("2026-10-07.md");
        std::fs::write(&path, "## 09:00:00\n\nFirst.\n\n## 10:00:00\n\nSecond.\n\n").unwrap();
        let item = day(&path).unwrap();
        assert_eq!(item.id, "dictation:2026-10-07");
        assert_eq!(item.title, "2 dictations");
        assert_eq!(item.at, local_ms("2026-10-07", "09:00:00").unwrap());
        assert_eq!(
            item.lines[1].start_ms,
            local_ms("2026-10-07", "10:00:00").unwrap()
        );
        assert_eq!(item.paste, "First.\n\nSecond.");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conflict_copy_has_its_own_library_item() {
        let root = temp_dir();
        let path = root.join("2026-10-13 2.md");
        std::fs::write(&path, "## 10:00:00\n\nSecond\n\n").unwrap();
        let item = day(&path).unwrap();
        assert_eq!(item.id, "dictation:2026-10-13 2");
        assert_eq!(item.lines[0].text, "Second");
        assert_eq!(item.files.markdown.as_deref(), Some(path.to_str().unwrap()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn meeting_lines_parse_minutes_and_hours() {
        let lines = meeting_lines("[01:23] You: Hello.\n[1:02:03] Them: Hi.\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start_ms, 83_000);
        assert_eq!(lines[0].speaker, Some("you"));
        assert_eq!(lines[1].start_ms, 3_723_000);
        assert_eq!(lines[1].speaker, Some("them"));
        assert_eq!(lines[1].text, "Hi.");
    }

    #[test]
    fn a_meeting_fix_changes_only_its_line() {
        let markdown = "# Zoom meeting\n\nOct 7.\n\n[00:05] You: we use superbase\n\n[00:05] Them: we use superbase\n";
        assert_eq!(
            fixed_meeting(markdown, 5_000, "we use superbase", "we use Supabase").as_deref(),
            Some("# Zoom meeting\n\nOct 7.\n\n[00:05] You: we use Supabase\n\n[00:05] Them: we use superbase\n")
        );
        assert_eq!(
            fixed_meeting(markdown, 6_000, "we use superbase", "x"),
            None,
            "a line that is no longer there is not rewritten"
        );
    }

    #[test]
    fn take_uses_real_capture_schema_and_sorts() {
        let root = temp_dir();
        let history = root.join("history");
        let documents = root.join("documents");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        std::fs::write(history.join("2026-10-06.md"), "## 09:00:00\n\nOld.\n\n").unwrap();
        let path = documents.join("2026-10-07-10-00-00");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("take.md"), "# Screen take").unwrap();
        std::fs::write(path.join("transcript.json"), r#"{"type":"see-computer.session","version":1,"durationMs":25000,"fullText":"Look at the button","segments":[{"startMs":1000,"endMs":2000,"text":"Look at the button"}],"captures":[{"type":"shot","atMs":3000,"file":"shots/001.png"},{"type":"clip","startMs":4000,"durationMs":5000,"video":"clips/001/clip.mov","frames":[{"atMs":4500,"file":"clips/001/frames/0004500.jpg"}]}]}"#).unwrap();
        let found = items_in(&history, &documents);
        assert_eq!(
            found
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["take:2026-10-07-10-00-00", "dictation:2026-10-06"]
        );
        assert_eq!(found[0].duration_ms, Some(25_000));
        assert_eq!(found[0].lines[0].end_ms, Some(2_000));
        assert_eq!(
            found[0]
                .media
                .iter()
                .map(|media| media.kind)
                .collect::<Vec<_>>(),
            vec!["shot", "clip", "frame"]
        );
        assert_eq!(
            found[0].media[1].poster.as_deref(),
            Some(path.join("clips/001/frames/0004500.jpg").to_str().unwrap())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
