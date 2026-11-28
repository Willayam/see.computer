//! What the engine has already read of a meeting, one line per chunk in the
//! meeting folder. Live lines write it while the call runs, so the notes
//! written after Stop read only the chunks it lacks, and a retry after a
//! failure reads only what failed. It is removed once the notes are done.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use crate::engine::Segment;

use super::chunks::Chunk;
use super::folder::HEARD;
use super::transcript::Speaker;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    track: Speaker,
    start: usize,
    end: usize,
    sum: u64,
    segments: Vec<Said>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Said {
    start_ms: u64,
    end_ms: u64,
    text: String,
}

/// Appends what `chunk` of `track` said, with segment times counted from the
/// start of the track.
pub fn keep(
    folder: &Path,
    track: Speaker,
    chunk: &Chunk,
    segments: &[Segment],
) -> std::io::Result<()> {
    let entry = Entry {
        track,
        start: chunk.start,
        end: chunk.end(),
        sum: chunk.sum(),
        segments: segments
            .iter()
            .map(|segment| Said {
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text: segment.text.clone(),
            })
            .collect(),
    };
    let mut line = serde_json::to_vec(&entry).map_err(std::io::Error::other)?;
    line.push(b'\n');
    // One write per line, so a crash can cut a line short but never mix two.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join(HEARD))?
        .write_all(&line)
}

/// Everything on record for a meeting.
#[derive(Default)]
pub struct Heard(HashMap<(Speaker, usize, usize, u64), Vec<Segment>>);

impl Heard {
    /// A line cut short by a crash is skipped, and so is the whole file while
    /// iCloud holds it, because reading an evicted file can stall for seconds.
    pub fn read(folder: &Path) -> Heard {
        let path = folder.join(HEARD);
        if crate::shadow::evicted(&path) {
            return Heard::default();
        }
        let text = std::fs::read_to_string(path).unwrap_or_default();
        Heard(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
                .map(|entry| {
                    let segments = entry
                        .segments
                        .into_iter()
                        .map(|said| Segment {
                            start_ms: said.start_ms,
                            end_ms: said.end_ms,
                            text: said.text,
                        })
                        .collect();
                    ((entry.track, entry.start, entry.end, entry.sum), segments)
                })
                .collect(),
        )
    }

    /// What this exact audio said, if the engine has read it before. A chunk
    /// that starts or ends elsewhere, or holds other samples, is not on record.
    pub fn find(&self, track: Speaker, chunk: &Chunk) -> Option<&[Segment]> {
        self.0
            .get(&(track, chunk.start, chunk.end(), chunk.sum()))
            .map(Vec::as_slice)
    }
}

pub fn forget(folder: &Path) {
    let _ = std::fs::remove_file(folder.join(HEARD));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &str) -> Segment {
        Segment {
            start_ms: 1_200,
            end_ms: 2_400,
            text: text.to_owned(),
        }
    }

    #[test]
    fn only_the_same_audio_at_the_same_place_is_on_record() {
        let folder = std::env::temp_dir().join(format!("see-heard-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        forget(&folder);
        let chunk = Chunk {
            start: 16_000,
            samples: vec![0.25; 8_000],
        };
        keep(&folder, Speaker::Them, &chunk, &[segment("Hej")]).unwrap();
        keep(&folder, Speaker::You, &chunk, &[]).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(folder.join(HEARD))
            .unwrap()
            .write_all(br#"{"track":"them","start":24000,"end":"#)
            .unwrap();

        let heard = Heard::read(&folder);
        assert_eq!(
            heard.find(Speaker::Them, &chunk),
            Some(&[segment("Hej")][..])
        );
        assert_eq!(heard.find(Speaker::You, &chunk), Some(&[][..]));
        let moved = Chunk {
            start: 16_001,
            samples: chunk.samples.clone(),
        };
        assert_eq!(heard.find(Speaker::Them, &moved), None);
        let longer = Chunk {
            start: 16_000,
            samples: vec![0.25; 8_001],
        };
        assert_eq!(heard.find(Speaker::Them, &longer), None);
        let mut other = chunk.samples.clone();
        other[4_000] = 0.26;
        let changed = Chunk {
            start: 16_000,
            samples: other,
        };
        assert_eq!(heard.find(Speaker::Them, &changed), None);

        forget(&folder);
        assert!(Heard::read(&folder).find(Speaker::Them, &chunk).is_none());
        std::fs::remove_dir_all(folder).unwrap();
    }
}
