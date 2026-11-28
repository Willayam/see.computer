//! Cuts a meeting track into the chunks the engine reads, holding at most one
//! chunk of audio at a time. Live lines and final notes cut the same track at
//! the same samples, so a chunk read while recording never has to be read again.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::engine::chunk_cut;

pub const RATE: usize = 16_000;
/// How much of a track comes off the disk at a time.
pub const READ: usize = 10 * RATE;
/// No speech fits inside one step of a 16-bit sample, so a sample this small
/// is digital silence: the zeros the tap pads pauses with, or a muted input.
const STILL: f32 = 1.5 / 32_768.0;
/// A run of digital silence this long is left out of every chunk, and the
/// chunk before it ends where the run begins. On 2026-11-28 the Swedish
/// fixture read with no errors alone and with 0.6 s of zeros inside it, lost
/// a word at 1 s, and came back as English once 5 s of zeros followed half a
/// sentence. Anything shorter or louder stays in, so a track with no such run
/// is cut exactly as `engine::Utterance` cuts a dictation.
const GAP: usize = RATE;
/// `chunk_cut` looks for a pause each time the audio waiting grows by this.
const LOOK: usize = RATE;
/// ExtAudioFile puts a filler chunk before the samples, which start at byte 4096.
const HEADER_MAX: u64 = 8192;

/// A stretch of a track short enough for the engine to read in one pass.
#[derive(Debug, PartialEq)]
pub struct Chunk {
    /// Where it starts in the track, in samples.
    pub start: usize,
    pub samples: Vec<f32>,
}

impl Chunk {
    pub fn end(&self) -> usize {
        self.start + self.samples.len()
    }

    pub fn start_ms(&self) -> u64 {
        (self.start * 1_000 / RATE) as u64
    }

    /// Tells this audio from any other at the same place in the track. FNV-1a,
    /// because the value is stored and `DefaultHasher` may change between
    /// Rust releases.
    pub fn sum(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for sample in &self.samples {
            for byte in sample.to_bits().to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }
}

/// Where the cuts fall depends on the samples alone, never on how many arrive
/// at a time.
#[derive(Default)]
pub struct Chunker {
    /// Audio since the last cut.
    waiting: Vec<f32>,
    start: usize,
    taken: usize,
    /// How many still samples `waiting` ends with.
    still: usize,
    /// Inside a run of stillness that has already reached `GAP`.
    silent: bool,
}

impl Chunker {
    pub fn push(&mut self, samples: &[f32]) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        for &sample in samples {
            let still = sample.abs() <= STILL;
            self.taken += 1;
            if self.silent {
                if still {
                    continue;
                }
                self.silent = false;
            }
            if self.waiting.is_empty() {
                self.start = self.taken - 1;
            }
            self.waiting.push(sample);
            if !still {
                self.still = 0;
            } else {
                self.still += 1;
                if self.still == GAP {
                    self.waiting.truncate(self.waiting.len() - GAP);
                    self.still = 0;
                    chunks.extend(self.take());
                    self.silent = true;
                    continue;
                }
            }
            if self.waiting.len() % LOOK == 0 {
                if let Some(at) = chunk_cut(&self.waiting) {
                    chunks.push(Chunk {
                        start: self.start,
                        samples: self.waiting.drain(..at).collect(),
                    });
                    self.start += at;
                    self.still = self.still.min(self.waiting.len());
                }
            }
        }
        chunks
    }

    /// The audio after the last cut, once the track has ended.
    pub fn finish(&mut self) -> Option<Chunk> {
        self.take()
    }

    /// The audio after the last cut, as a chunk that may still grow.
    pub fn unfinished(&self) -> Option<Chunk> {
        self.voiced().then(|| Chunk {
            start: self.start,
            samples: self.waiting.clone(),
        })
    }

    /// Whether anything but stillness is waiting.
    fn voiced(&self) -> bool {
        self.still < self.waiting.len()
    }

    fn take(&mut self) -> Option<Chunk> {
        let voiced = self.voiced();
        self.still = 0;
        let samples = std::mem::take(&mut self.waiting);
        voiced.then_some(Chunk {
            start: self.start,
            samples,
        })
    }
}

/// Reads a 16 kHz mono 16-bit WAV as it grows. hound and ExtAudioFile leave
/// the data chunk size at 0 until the file is finished, and hound wraps it past
/// 4 GB, so the audio runs to the end of the file whatever the header says.
pub struct Tail {
    path: PathBuf,
    data: Option<u64>,
    read: u64,
}

impl Tail {
    pub fn new(path: PathBuf) -> Tail {
        Tail {
            path,
            data: None,
            read: 0,
        }
    }

    /// Whether the file has shown a WAV header yet.
    pub fn opened(&self) -> bool {
        self.data.is_some()
    }

    /// The next samples, at most `limit` of them, or none when nothing new is
    /// in the file.
    pub fn read(&mut self, limit: usize) -> io::Result<Vec<f32>> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let data = match self.data {
            Some(data) => data,
            None => match data_start(&mut file)? {
                Some(data) => *self.data.insert(data),
                None => return Ok(Vec::new()),
            },
        };
        let from = data + self.read;
        let length = (file.metadata()?.len().saturating_sub(from) & !1).min(limit as u64 * 2);
        let mut bytes = vec![0; length as usize];
        file.seek(SeekFrom::Start(from))?;
        file.read_exact(&mut bytes)?;
        self.read += length;
        Ok(bytes
            .chunks_exact(2)
            .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0)
            .collect())
    }
}

fn data_start(file: &mut File) -> io::Result<Option<u64>> {
    let mut header = Vec::new();
    file.take(HEADER_MAX).read_to_end(&mut header)?;
    if header.len() < 12 || &header[..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Ok(None);
    }
    let mut at = 12;
    while at + 8 <= header.len() {
        let id = &header[at..at + 4];
        let size = u32::from_le_bytes(header[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        if id == b"data" {
            return Ok(Some(body as u64));
        }
        if id == b"fmt " && body + 16 <= header.len() {
            let field = |at: usize| u16::from_le_bytes([header[body + at], header[body + at + 1]]);
            let rate = u32::from_le_bytes(header[body + 4..body + 8].try_into().unwrap());
            if field(2) != 1 || rate != RATE as u32 || field(14) != 16 {
                return Err(io::Error::other("the WAV is not 16 kHz mono 16-bit"));
            }
        }
        at = body + size + (size & 1);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, EngineError, Transcription, Utterance};
    use crate::mic::Audio16k;

    /// A voice-like tone that never drops under the stillness floor.
    fn voice(samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|index| 0.2 + 0.1 * (index as f32 * 0.05).sin())
            .collect()
    }

    fn cut_in_blocks(track: &[f32], block: usize) -> Vec<Chunk> {
        let mut chunker = Chunker::default();
        let mut chunks = Vec::new();
        for samples in track.chunks(block) {
            chunks.extend(chunker.push(samples));
        }
        chunks.extend(chunker.finish());
        chunks
    }

    fn spans(chunks: &[Chunk]) -> Vec<(usize, usize)> {
        chunks
            .iter()
            .map(|chunk| (chunk.start, chunk.end()))
            .collect()
    }

    #[test]
    fn stillness_separates_chunks_and_is_never_read() {
        let mut track = vec![0.0; 8 * RATE];
        track.extend(voice(6 * RATE));
        track.extend(vec![0.0; 15 * RATE]);
        track.extend(voice(3 * RATE));
        track.extend(vec![0.0; 5 * RATE]);
        let chunks = cut_in_blocks(&track, RATE);
        assert_eq!(
            spans(&chunks),
            [(8 * RATE, 14 * RATE), (29 * RATE, 32 * RATE)]
        );
        assert_eq!(chunks[0].samples, voice(6 * RATE));
        assert_eq!(chunks[1].start_ms(), 29_000);
    }

    #[test]
    fn stillness_shorter_than_the_gap_stays_where_it_is() {
        let mut track = vec![0.0; GAP - 1];
        track.extend(voice(2 * RATE));
        track.extend(vec![0.0; GAP - 1]);
        track.extend(voice(2 * RATE));
        track.extend(vec![0.0; GAP - 1]);
        let chunks = cut_in_blocks(&track, 4_000);
        assert_eq!(spans(&chunks), [(0, track.len())]);
        assert_eq!(chunks[0].samples, track);
    }

    #[derive(Default)]
    struct Lengths(Vec<usize>);

    impl Engine for Lengths {
        fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError> {
            self.0.push(audio.samples().len());
            Ok(Transcription::empty())
        }
    }

    #[test]
    fn a_track_with_no_long_stillness_is_cut_as_a_dictation_is() {
        let mut track = vec![0.0; RATE / 2];
        track.extend(voice(75 * RATE));
        for breath in (9 * RATE..track.len()).step_by(9 * RATE) {
            track[breath - 4_800..breath].fill(0.002);
        }
        track[33 * RATE..33 * RATE + GAP - 1].fill(0.0);
        for lone in (777..track.len()).step_by(1_000) {
            track[lone] = 0.0;
        }

        let mut dictation = Lengths::default();
        let mut utterance = Utterance::default();
        for second in track.chunks(RATE) {
            utterance.feed(&mut dictation, &Audio16k::from_samples(second.to_vec()));
        }
        utterance
            .finish(&mut dictation, &Audio16k::from_samples(Vec::new()))
            .unwrap();

        let chunks = cut_in_blocks(&track, 7_001);
        assert!(chunks.len() > 3, "{:?}", spans(&chunks));
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.samples.len())
                .collect::<Vec<_>>(),
            dictation.0
        );
        assert_eq!(chunks[0].start, 0);
        let read: Vec<f32> = chunks.into_iter().flat_map(|chunk| chunk.samples).collect();
        assert_eq!(read, track);
    }

    #[test]
    fn the_same_track_cuts_the_same_however_it_arrives() {
        let mut track = vec![0.0; 3 * RATE];
        for seconds in [7, 1, 26, 2, 41, 9] {
            let mut talk = voice(seconds * RATE);
            for pause in (12 * RATE..talk.len()).step_by(12 * RATE) {
                talk[pause - 4_800..pause].fill(0.002);
            }
            track.extend(talk);
            track.extend(vec![0.0; seconds * RATE / 3 + 1]);
        }
        let whole = cut_in_blocks(&track, track.len());
        let touching = |pair: &[Chunk]| pair[0].end() == pair[1].start;
        assert!(whole.windows(2).any(touching), "a cut at a breath");
        assert!(!whole.windows(2).all(touching), "a cut at a silence");
        assert!(whole.iter().all(|chunk| chunk.samples.len() <= 30 * RATE));
        for block in [1_600, 3_217, 16_000, 95_999] {
            assert_eq!(cut_in_blocks(&track, block), whole, "block of {block}");
        }
    }

    #[test]
    fn speech_just_above_the_floor_is_all_kept() {
        let quiet: Vec<f32> = (0..50 * RATE)
            .map(|index| if index % 2 == 0 { 2.0 } else { -2.0 } / 32_768.0)
            .collect();
        let chunks = cut_in_blocks(&quiet, RATE);
        assert_eq!(chunks[0].start, 0);
        assert_eq!(chunks.last().unwrap().end(), quiet.len());
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].end(), pair[1].start);
        }
    }

    #[test]
    fn an_hour_of_silence_holds_no_audio_and_yields_no_chunk() {
        let mut chunker = Chunker::default();
        let second = vec![0.0; RATE];
        for _ in 0..3_600 {
            assert!(chunker.push(&second).is_empty());
            assert!(chunker.waiting.is_empty());
        }
        assert_eq!(chunker.finish(), None);
    }

    #[test]
    fn the_sum_follows_the_audio() {
        let chunk = |start, samples| Chunk { start, samples };
        let one = chunk(0, voice(RATE));
        let mut changed = voice(RATE);
        changed[500] += 1.0 / 32_768.0;
        assert_eq!(one.sum(), chunk(9, voice(RATE)).sum());
        assert_ne!(one.sum(), chunk(0, changed).sum());
    }

    #[test]
    fn a_growing_file_is_read_a_bounded_block_at_a_time() {
        let path = std::env::temp_dir().join(format!(
            "see-meeting-tail-{}-{}.wav",
            std::process::id(),
            line!()
        ));
        let mut tail = Tail::new(path.clone());
        assert!(tail.read(RATE).unwrap().is_empty());
        assert!(!tail.opened());
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for sample in 0..40_000_i32 {
            writer.write_sample((sample % 1_000) as i16).unwrap();
        }
        writer.flush().unwrap();
        let first = tail.read(RATE).unwrap();
        assert!(tail.opened());
        assert_eq!(first.len(), RATE);
        assert_eq!(first[999], 999.0 / 32_768.0);
        assert_eq!(tail.read(10 * RATE).unwrap().len(), 40_000 - RATE);
        assert!(tail.read(RATE).unwrap().is_empty());
        writer.write_sample(7_i16).unwrap();
        writer.finalize().unwrap();
        assert_eq!(tail.read(RATE).unwrap(), [7.0 / 32_768.0]);
        std::fs::remove_file(path).unwrap();
    }
}
