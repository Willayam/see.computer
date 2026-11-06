//! Dictations whose transcription outran its deadline, kept as audio until the
//! engine delivers their words or the user asks for another pass.
//!
//! A file here means words that have not reached the user yet. The controller
//! removes it once a transcription of it comes back, with text or without.

use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Saved {
    root: PathBuf,
}

impl Saved {
    pub fn at(root: PathBuf) -> Saved {
        Saved { root }
    }

    pub fn keep(&self, audio: &crate::mic::Audio16k) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.root)?;
        let name = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f");
        let path = self.root.join(format!("{name}.wav"));
        crate::ledger::write_audio(&path, audio).map_err(std::io::Error::other)?;
        Ok(path)
    }

    /// Oldest first, so a retry delivers them in the order they were spoken.
    pub fn list(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
            .collect();
        paths.sort();
        paths
    }

    pub fn remove(&self, path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    /// Deletes the oldest files beyond `keep`.
    pub fn trim(&self, keep: usize) {
        let paths = self.list();
        for path in &paths[..paths.len().saturating_sub(keep)] {
            self.remove(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_audio_lists_in_order_and_reads_back() {
        let root = std::env::temp_dir().join(format!("see-saved-{}", std::process::id()));
        let saved = Saved::at(root.clone());
        let first = saved
            .keep(&crate::mic::Audio16k::from_samples(vec![0.25; 1600]))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = saved.keep(&crate::mic::Audio16k::silence(0.1)).unwrap();
        assert_eq!(saved.list(), vec![first.clone(), second]);
        let audio = crate::mic::Audio16k::from_wav(&first).unwrap();
        assert_eq!(audio.samples().len(), 1600);
        assert!((audio.samples()[0] - 0.25).abs() < 0.001);
        saved.remove(&first);
        assert_eq!(saved.list().len(), 1);
        let third = saved.keep(&crate::mic::Audio16k::silence(0.1)).unwrap();
        saved.trim(1);
        assert_eq!(saved.list(), vec![third]);
        let _ = std::fs::remove_dir_all(root);
    }
}
