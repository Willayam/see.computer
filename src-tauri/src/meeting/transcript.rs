use chrono::{Local, TimeZone};

use crate::engine::Segment;
use crate::meeting::folder::Manifest;

const ECHO_SLACK_MS: u64 = 750;
const ECHO_WORD_PERCENT: usize = 60;
const JOIN_GAP_MS: u64 = 1_500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    You,
    Them,
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

fn timestamp(ms: u64) -> String {
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
            let speaker = match line.speaker {
                Speaker::You => "You",
                Speaker::Them => "Them",
            };
            output.push_str(&format!(
                "[{}] {speaker}: {}\n",
                timestamp(line.start_ms),
                line.text
            ));
        }
    }
    output
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
    fn renders_header_body_and_hour_timestamp() {
        let manifest = Manifest {
            app: Some("Zoom".to_owned()),
            started_ms: 1_781_000_000_000,
            ended_ms: Some(1_781_002_520_000),
            state: State::Done,
            error: None,
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
