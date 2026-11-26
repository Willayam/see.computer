//! Stand-ins for the Mac modules the shared engine reaches into.

pub mod paths {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    static ROOTS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

    /// The folder the model downloads into, and the folder holding the
    /// dictionary files. Set once, before the engine starts.
    pub fn set(models: PathBuf, dictionary: PathBuf) {
        let _ = ROOTS.set((models, dictionary));
    }

    pub fn models() -> PathBuf {
        ROOTS.get().expect("paths::set runs first").0.clone()
    }

    pub fn documents() -> PathBuf {
        ROOTS.get().expect("paths::set runs first").1.clone()
    }

    pub fn history_fallback() -> PathBuf {
        documents()
    }
}

pub mod mic {
    pub struct Audio16k(Vec<f32>);

    impl Audio16k {
        pub(crate) fn from_samples(samples: Vec<f32>) -> Self {
            Self(samples)
        }

        pub fn silence(seconds: f32) -> Self {
            Self(vec![0.0; (seconds * 16_000.0) as usize])
        }

        pub fn samples(&self) -> &[f32] {
            &self.0
        }
    }
}

pub mod learn {
    use std::ffi::{c_char, CString};
    use std::sync::OnceLock;

    pub type Knows = extern "C" fn(*const c_char) -> bool;

    static KNOWS: OnceLock<Knows> = OnceLock::new();

    pub struct Words {
        #[cfg(test)]
        list: Option<std::collections::HashSet<String>>,
    }

    impl Words {
        pub fn set_knows(knows: Option<Knows>) {
            if let Some(knows) = knows {
                let _ = KNOWS.set(knows);
            }
        }

        pub fn shared() -> &'static Words {
            static WORDS: Words = Words {
                #[cfg(test)]
                list: None,
            };
            &WORDS
        }

        #[cfg(test)]
        pub fn from_list(words: &[&str]) -> Self {
            Self {
                list: Some(words.iter().map(|word| (*word).to_owned()).collect()),
            }
        }

        pub fn knows(&self, word: &str) -> bool {
            #[cfg(test)]
            if let Some(list) = &self.list {
                return list.contains(word) || list.contains(&word.to_lowercase());
            }
            match (KNOWS.get(), CString::new(word)) {
                (Some(knows), Ok(word)) => knows(word.as_ptr()),
                _ => true,
            }
        }

        pub fn knows_quickly(&self, word: &str) -> bool {
            self.knows(word)
        }

        pub fn warm_quickly(&self, _words: &[String]) {}
    }
}

/// The app reads its own container, where nothing is ever evicted to iCloud.
pub mod library {
    pub fn request(_paths: &[std::path::PathBuf]) {}
}

pub mod shadow {
    pub enum Text {
        Current(String),
        Stale(String),
        Waiting,
        Missing,
    }

    pub fn read(path: &std::path::Path) -> Text {
        match std::fs::read_to_string(path) {
            Ok(text) => Text::Current(text),
            Err(_) => Text::Missing,
        }
    }
}
