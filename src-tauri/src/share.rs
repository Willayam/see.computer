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
                let plain = recording.path.to_string_lossy().into_owned();
                let rendered = if recording.path.is_absolute() {
                    url::Url::from_file_path(&recording.path)
                        .map(|url| url.to_string())
                        .unwrap_or(plain)
                } else {
                    plain
                };
                Link(Text::literal(rendered))
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
