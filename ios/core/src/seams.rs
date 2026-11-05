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

/// The phone reads no on-screen words yet, so there is nothing to correct against.
pub mod context {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Term {
        pub text: String,
        pub source: &'static str,
    }

    pub fn near_misses(
        text: &str,
        _terms: &[Term],
        _saved: &[&str],
        _words: &crate::learn::Words,
    ) -> String {
        text.to_owned()
    }
}

pub mod learn {
    pub struct Words;

    impl Words {
        pub fn shared() -> &'static Words {
            &Words
        }
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
