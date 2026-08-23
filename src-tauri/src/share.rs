//! Turn a finished recording into something to paste.

use crate::paste::Text;
use crate::recorder::Recording;

pub enum Share {
    LocalFile,
}

impl Share {
    pub fn link(&self, recording: &Recording) -> Link {
        match self {
            Share::LocalFile => {
                let url = url::Url::from_file_path(&recording.path)
                    .expect("recording paths are absolute local paths");
                let text = Text::parse(url.to_string()).expect("file URLs are non-empty");
                Link(text)
            }
        }
    }
}

pub struct Link(Text);

impl Link {
    pub fn into_text(self) -> Text {
        self.0
    }
}
