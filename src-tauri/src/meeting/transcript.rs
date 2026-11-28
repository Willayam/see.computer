use chrono::{Local, TimeZone};

use crate::engine::Segment;
use crate::meeting::folder::{Manifest, THEM, YOU};

const ECHO_SLACK_MS: u64 = 750;
const ECHO_WORD_PERCENT: usize = 60;
const JOIN_GAP_MS: u64 = 1_500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Speaker {
    You,
    Them,
}

impl Speaker {
    pub fn wav(self) -> &'static str {
        match self {
            Speaker::You => YOU,
            Speaker::Them => THEM,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Speaker::You => "You",
            Speaker::Them => "Them",
        }
    }
}

/// Segments timed from the start of their track, moved to where the track
/// sits in the meeting.
pub fn shifted(mut segments: Vec<Segment>, by_ms: u64) -> Vec<Segment> {
    for segment in &mut segments {
        segment.start_ms += by_ms;
        segment.end_ms += by_ms;
    }
    segments
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub speaker: Speaker,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|ch| !ch.is_ascii_punctuation())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

fn is_echo(you: &Segment, them: &[Segment]) -> bool {
    let yours = words(&you.text);
    if yours.is_empty() {
        return true;
    }
    them.iter().any(|other| {
        if you.start_ms > other.end_ms.saturating_add(ECHO_SLACK_MS)
            || other.start_ms > you.end_ms.saturating_add(ECHO_SLACK_MS)
        {
            return false;
        }
        let theirs = words(&other.text);
        yours.iter().filter(|word| theirs.contains(word)).count() * 100
            >= yours.len() * ECHO_WORD_PERCENT
    })
}

pub fn merge(you: &[Segment], them: &[Segment]) -> Vec<Line> {
    let mut lines = you
        .iter()
        .filter(|segment| !is_echo(segment, them))
        .map(|segment| Line {
            speaker: Speaker::You,
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            text: segment.text.clone(),
        })
        .chain(them.iter().map(|segment| Line {
            speaker: Speaker::Them,
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            text: segment.text.clone(),
        }))
        .collect::<Vec<_>>();
    lines.sort_by_key(|line| (line.start_ms, line.speaker == Speaker::You));
    let mut joined: Vec<Line> = Vec::new();
    for line in lines {
        if let Some(last) = joined.last_mut() {
            if last.speaker == line.speaker
                && line.start_ms.saturating_sub(last.end_ms) < JOIN_GAP_MS
            {
                last.text.push(' ');
                last.text.push_str(&line.text);
                last.end_ms = last.end_ms.max(line.end_ms);
                continue;
            }
        }
        joined.push(line);
    }
    joined
}

pub(super) fn timestamp(ms: u64) -> String {
    let seconds = ms / 1_000;
    if seconds >= 3_600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3_600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

pub fn render(manifest: &Manifest, lines: &[Line]) -> String {
    let started = Local.timestamp_millis_opt(manifest.started_ms).single();
    let date = started.map_or_else(
        || "Unknown date".to_owned(),
        |at| at.format("%b %-d, %Y, %H:%M").to_string(),
    );
    let when = match manifest.ended_ms {
        Some(end) => {
            let end_time = Local.timestamp_millis_opt(end).single();
            let end_time =
                end_time.map_or_else(|| "unknown".to_owned(), |at| at.format("%H:%M").to_string());
            let duration = manifest.duration_ms().unwrap_or(0) / 60_000;
            let length = if duration == 0 {
                "under 1 min".to_owned()
            } else {
                format!("{duration} min")
            };
            format!("{date} to {end_time} ({length}).")
        }
        None => format!("{date} (still recording)."),
    };
    let mut output = format!(
        "# {}\n\n{when} \"You\" is this Mac's microphone and \"Them\" is the call audio.\n\n",
        manifest
            .app
            .as_ref()
            .map_or_else(|| "Meeting".to_owned(), |app| format!("{app} meeting"))
    );
    if lines.is_empty() {
        output.push_str("Nothing was transcribed.\n");
    } else {
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                output.push('\n');
            }
            output.push_str(&format!(
                "[{}] {}: {}\n",
                timestamp(line.start_ms),
                line.speaker.label(),
                line.text
            ));
        }
    }
    output
}

/// A transcript line as where its words begin, or `None` for any other line.
fn words_at(line: &str) -> Option<usize> {
    let (clock, rest) = line.strip_prefix('[')?.split_once("] ")?;
    [Speaker::You, Speaker::Them]
        .into_iter()
        .find_map(|speaker| {
            rest.strip_prefix(speaker.label())?.strip_prefix(": ")?;
            Some(clock.len() + 3 + speaker.label().len() + 2)
        })
}

/// A line's time and speaker, and how many lines before it share them.
fn keyed(text: &str) -> Vec<((&str, usize), &str)> {
    let mut seen = std::collections::HashMap::new();
    text.lines()
        .filter_map(|line| {
            let (key, words) = line.split_at(words_at(line)?);
            let nth = seen.entry(key).or_insert(0_usize);
            *nth += 1;
            Some(((key, *nth), words))
        })
        .collect()
}

/// `fresh` with the user's corrections put back. `written` is what the app
/// last wrote and `current` is that file as it reads now, so a line that
/// differs between them was corrected by hand. A correction carries over only
/// when the engine heard the same words at the same time again, since a line
/// the engine now reads differently is no longer the line that was corrected.
pub fn carry_corrections(written: &str, current: &str, fresh: &str) -> String {
    let written: std::collections::HashMap<_, _> = keyed(written).into_iter().collect();
    let corrected: std::collections::HashMap<_, _> = keyed(current)
        .into_iter()
        .filter(|(key, words)| written.get(key).is_some_and(|before| before != words))
        .collect();
    let mut keys = keyed(fresh).into_iter();
    let mut out = String::with_capacity(fresh.len());
    for line in fresh.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        let kept = words_at(body).and_then(|at| {
            let (key, words) = keys.next()?;
            let theirs = corrected.get(&key)?;
            (written.get(&key) == Some(&words)).then(|| format!("{}{theirs}", &body[..at]))
        });
        match kept {
            Some(kept) => {
                out.push_str(&kept);
                out.push_str(&line[body.len()..]);
            }
            None => out.push_str(line),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::folder::State;

    fn segment(start_ms: u64, end_ms: u64, text: &str) -> Segment {
        Segment {
            start_ms,
            end_ms,
            text: text.to_owned(),
        }
    }

    #[test]
    fn drops_echo_but_keeps_distinct_and_distant_speech() {
        let you = [
            segment(1000, 2000, "Hello there friend."),
            segment(3000, 3500, "I agree"),
            segment(8000, 8500, "Hello there friend"),
        ];
        let them = [
            segment(900, 1900, "Hello there friend"),
            segment(3000, 3400, "Different words"),
        ];
        let lines = merge(&you, &them);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.speaker == Speaker::You)
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["I agree", "Hello there friend"]
        );
    }

    #[test]
    fn orders_and_joins_same_speaker() {
        let lines = merge(
            &[segment(5000, 5500, "Hi"), segment(6000, 6500, "again")],
            &[segment(1000, 1500, "First")],
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].speaker, Speaker::Them);
        assert_eq!(lines[1].text, "Hi again");
        assert_eq!(lines[1].end_ms, 6500);
    }

    #[test]
    fn corrections_survive_a_pass_that_hears_the_same_words() {
        let written = "# Zoom meeting\n\nHeader.\n\n[00:01] You: Helo there.\n\n[00:04] Them: Fine thanks.\n\n[00:04] Them: And you.\n\n[00:09] You: Good by.\n";
        let current = written
            .replace("Helo there.", "Hello there.")
            .replace("And you.", "And you?")
            .replace("Good by.", "Goodbye.");
        assert_eq!(carry_corrections(written, &current, written), current);

        let fresh = written.replace("Good by.", "Good buy.");
        assert_eq!(
            carry_corrections(written, &current, &fresh),
            current.replace("Goodbye.", "Good buy."),
            "a line the engine now reads differently takes the new reading"
        );
        assert_eq!(carry_corrections(written, written, &fresh), fresh);
    }

    #[test]
    fn renders_header_body_and_hour_timestamp() {
        let manifest = Manifest {
            app: Some("Zoom".to_owned()),
            started_ms: 1_781_000_000_000,
            ended_ms: Some(1_781_002_520_000),
            state: State::Done,
            error: None,
            tracks: None,
        };
        let date = Local
            .timestamp_millis_opt(manifest.started_ms)
            .single()
            .unwrap()
            .format("%b %-d, %Y, %H:%M")
            .to_string();
        let end = Local
            .timestamp_millis_opt(manifest.ended_ms.unwrap())
            .single()
            .unwrap()
            .format("%H:%M")
            .to_string();
        assert_eq!(render(&manifest, &[Line { speaker: Speaker::You, start_ms: 3_000, end_ms: 4_000, text: "Hi.".to_owned() }, Line { speaker: Speaker::Them, start_ms: 3_609_000, end_ms: 3_610_000, text: "Hello.".to_owned() }]), format!("# Zoom meeting\n\n{date} to {end} (42 min). \"You\" is this Mac's microphone and \"Them\" is the call audio.\n\n[00:03] You: Hi.\n\n[1:00:09] Them: Hello.\n"));
    }
}
