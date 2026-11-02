//! When the disk fills, macOS moves files in an iCloud Documents folder to the
//! cloud and leaves a placeholder. Reading one waits for its download, or fails
//! while the eviction is still running. A placeholder keeps its size and
//! modified time and `stat` does not download it.

use std::fs::{self, Metadata, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

#[derive(Debug, PartialEq)]
pub enum Text {
    Current(String),
    Stale(String),
    Waiting,
    Missing,
}

impl Text {
    pub fn text(self) -> Option<String> {
        match self {
            Self::Current(text) | Self::Stale(text) => Some(text),
            Self::Waiting | Self::Missing => None,
        }
    }
}

fn copy_path(source: &Path) -> PathBuf {
    let absolute = std::path::absolute(source).expect("absolute shadow source");
    crate::paths::shadow().join(absolute.strip_prefix("/").unwrap())
}

fn identity(meta: &Metadata) -> Option<(u64, SystemTime)> {
    Some((meta.len(), meta.modified().ok()?))
}

const SF_DATALESS: u32 = 0x4000_0000;

fn dataless(meta: &Metadata) -> bool {
    use std::os::darwin::fs::MetadataExt;
    meta.st_flags() & SF_DATALESS != 0
}

pub fn evicted(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| dataless(&meta))
}

#[derive(Hash)]
pub struct Stamp {
    source: Option<(u64, SystemTime)>,
    behind: bool,
}

pub fn stamp(source: &Path) -> Stamp {
    let meta = fs::symlink_metadata(source).ok();
    Stamp {
        source: meta.as_ref().and_then(identity),
        behind: meta.as_ref().is_some_and(|meta| behind_copy(source, meta)),
    }
}

pub fn behind(source: &Path) -> bool {
    fs::symlink_metadata(source).is_ok_and(|meta| behind_copy(source, &meta))
}

fn behind_copy(source: &Path, meta: &Metadata) -> bool {
    dataless(meta)
        && fs::symlink_metadata(copy_path(source))
            .ok()
            .as_ref()
            .and_then(identity)
            != identity(meta)
}

fn fallback(source: bool, evicted: bool, copy: Option<String>) -> Text {
    match (source, evicted, copy) {
        (false, _, _) => Text::Missing,
        (true, _, Some(text)) => Text::Stale(text),
        (true, true, None) => Text::Waiting,
        (true, false, None) => Text::Missing,
    }
}

pub fn read(source: &Path) -> Text {
    let copy = copy_path(source);
    let meta = match fs::symlink_metadata(source) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Text::Missing,
        Err(_) => return fallback(true, false, fs::read_to_string(copy).ok()),
    };
    let copied = fs::symlink_metadata(&copy).ok();
    if identity(&meta).is_some() && copied.as_ref().and_then(identity) == identity(&meta) {
        if let Ok(text) = fs::read_to_string(&copy) {
            return Text::Current(text);
        }
    }
    if !dataless(&meta) {
        if let Ok(text) = fs::read_to_string(source) {
            if fs::symlink_metadata(source)
                .ok()
                .as_ref()
                .and_then(identity)
                == identity(&meta)
            {
                let _ = save(&copy, &text, &meta);
            }
            return Text::Current(text);
        }
    }
    fallback(true, dataless(&meta), fs::read_to_string(copy).ok())
}

fn save(copy: &Path, text: &str, meta: &Metadata) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(copy.parent().unwrap())?;
    let temp = copy.with_file_name(format!(
        ".see-shadow-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.set_modified(meta.modified()?)?;
        fs::rename(&temp, copy)
    })();
    let _ = fs::remove_file(temp);
    result
}

pub fn prune() {
    prune_in(&crate::paths::shadow(), &crate::paths::shadow());
}

fn prune_in(dir: &Path, root: &Path) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            prune_in(&path, root);
        } else {
            let source = Path::new("/").join(path.strip_prefix(root).unwrap());
            let temp = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".see-shadow-"));
            if temp
                || fs::symlink_metadata(source)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                let _ = fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_copy_tracks_source_and_prunes_deleted_files() {
        let root = std::env::temp_dir().join(format!("shadow-source-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("day.md");
        let kept = root.join("kept.md");
        fs::write(&source, "First").unwrap();
        fs::write(&kept, "Keep").unwrap();
        assert_eq!(read(&source), Text::Current("First".into()));
        let copy = copy_path(&source);
        assert_eq!(
            identity(&fs::symlink_metadata(&source).unwrap()),
            identity(&fs::symlink_metadata(&copy).unwrap())
        );
        let modified = fs::metadata(&copy).unwrap().modified().unwrap();
        assert_eq!(read(&source), Text::Current("First".into()));
        assert_eq!(fs::metadata(&copy).unwrap().modified().unwrap(), modified);
        fs::write(&source, "Second version").unwrap();
        assert_eq!(read(&source), Text::Current("Second version".into()));
        assert_eq!(fs::read_to_string(&copy).unwrap(), "Second version");
        read(&kept);
        prune();
        assert!(copy.exists());
        fs::remove_file(&source).unwrap();
        assert_eq!(read(&source), Text::Missing);
        prune();
        assert!(!copy.exists());
        assert!(copy_path(&kept).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn what_an_unreadable_source_falls_back_to() {
        let old = || Some("old".to_owned());
        assert_eq!(fallback(false, false, None), Text::Missing);
        assert_eq!(fallback(false, true, old()), Text::Missing);
        assert_eq!(fallback(true, true, None), Text::Waiting);
        assert_eq!(fallback(true, true, old()), Text::Stale("old".into()));
        assert_eq!(fallback(true, false, old()), Text::Stale("old".into()));
        assert_eq!(fallback(true, false, None), Text::Missing);
    }
}
