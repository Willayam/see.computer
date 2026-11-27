use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate};
use std::io::{self, Write};
use std::time::Duration;

/// Words and dictations, for one day or summed over several.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    pub words: u64,
    pub dictations: u64,
    pub timed_words: u64,
    pub spoken_ms: u64,
}

impl Count {
    pub fn wpm(&self) -> Option<u64> {
        (self.spoken_ms >= 60_000).then(|| {
            ((u128::from(self.timed_words) * 60_000 + u128::from(self.spoken_ms) / 2)
                / u128::from(self.spoken_ms)) as u64
        })
    }
}
impl std::ops::AddAssign for Count {
    fn add_assign(&mut self, other: Self) {
        self.words += other.words;
        self.dictations += other.dictations;
        self.timed_words += other.timed_words;
        self.spoken_ms += other.spoken_ms;
    }
}
pub fn log_speed(path: &Path, at: DateTime<Local>, spoken: Duration, words: u64) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(
        file,
        "{}\t{}\t{}",
        at.timestamp_millis(),
        spoken.as_millis(),
        words
    )
}

/// Every day that has history, and the day files still in iCloud with no local copy.
#[derive(Clone, Debug, Default)]
pub struct Tally {
    pub days: BTreeMap<NaiveDate, Count>,
    pub waiting: Vec<PathBuf>,
}

pub fn words(text: &str) -> u64 {
    text.split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .count() as u64
}

pub type DayCounts = BTreeMap<PathBuf, (crate::shadow::Stamp, Count)>;

impl Tally {
    pub fn read(root: &Path, speed_log: &Path) -> Tally {
        Self::read_cached(root, speed_log, &mut BTreeMap::new())
    }

    pub fn read_cached(root: &Path, speed_log: &Path, cache: &mut DayCounts) -> Tally {
        let mut tally = Self::default();
        let mut previous = std::mem::take(cache);
        for (date, path) in crate::history::day_files(root) {
            let stamp = crate::shadow::stamp(&path);
            let count = previous
                .remove(&path)
                .filter(|(old, _)| old == &stamp)
                .map(|(_, c)| c)
                .or_else(|| {
                    let text = crate::shadow::read(&path).text()?;
                    let mut count = Count::default();
                    for entry in crate::history::day_entries(date, &text) {
                        if !entry.text.trim().is_empty() {
                            count.dictations += 1;
                            count.words += words(&entry.text);
                        }
                    }
                    Some(count)
                });
            if let Some(count) = count {
                if count.dictations > 0 {
                    *tally.days.entry(date).or_default() += count;
                }
                cache.insert(path, (stamp, count));
            } else {
                tally.waiting.push(path);
            }
        }
        if let Ok(text) = std::fs::read_to_string(speed_log) {
            for line in text.lines() {
                let fields: Vec<_> = line.split('\t').collect();
                if fields.len() != 3 {
                    continue;
                }
                if let (Ok(ms), Ok(spoken_ms), Ok(timed_words)) = (
                    fields[0].parse::<i64>(),
                    fields[1].parse::<u64>(),
                    fields[2].parse::<u64>(),
                ) {
                    if let Some(at) = DateTime::from_timestamp_millis(ms) {
                        *tally
                            .days
                            .entry(at.with_timezone(&Local).date_naive())
                            .or_default() += Count {
                            timed_words,
                            spoken_ms,
                            ..Count::default()
                        };
                    }
                }
            }
        }
        tally
    }

    pub fn total(&self) -> Count {
        self.since(NaiveDate::MIN)
    }

    pub fn on(&self, date: NaiveDate) -> Count {
        self.days.get(&date).copied().unwrap_or_default()
    }

    pub fn since(&self, from: NaiveDate) -> Count {
        self.days
            .range(from..)
            .fold(Count::default(), |mut sum, (_, count)| {
                sum += *count;
                sum
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_round_trip() {
        use chrono::TimeZone;
        assert_eq!(
            Count {
                timed_words: 240,
                spoken_ms: 120_000,
                ..Count::default()
            }
            .wpm(),
            Some(120)
        );
        assert_eq!(
            Count {
                timed_words: 240,
                spoken_ms: 59_999,
                ..Count::default()
            }
            .wpm(),
            None
        );
        let root = std::env::temp_dir().join(format!("speed-round-trip-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("speed.tsv");
        let at = Local.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap();
        log_speed(&path, at, Duration::from_secs(120), 240).unwrap();
        let tally = Tally::read(&root, &path);
        assert_eq!(
            tally.on(at.date_naive()),
            Count {
                words: 0,
                dictations: 0,
                timed_words: 240,
                spoken_ms: 120_000
            }
        );
        assert_eq!(tally.total().wpm(), Some(120));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn counts_words() {
        assert_eq!(words("Hello, world."), 2);
        assert_eq!(words("Det här är ord för ord, på svenska."), 8);
        assert_eq!(words("— 3 pm"), 2);
        assert_eq!(words("a - b"), 2);
        assert_eq!(words(""), 0);
        assert_eq!(words("  \n "), 0);
    }

    #[test]
    fn reads_history() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("see-tally-test-{unique}"));
        std::fs::create_dir_all(&root).unwrap();
        for (name, text) in [
            (
                "2026-10-13.md",
                "## 10:00:00\n\nHello, world.\n\n## 11:00:00\n\na - b\n\n",
            ),
            ("2026-10-13 2.md", "## 12:00:00\n\n— 3 pm\n\n"),
            (
                "2026-10-14.md",
                "## 10:00:00\n\nDet här är ord för ord, på svenska.\n\n## 11:00:00\n\n\n\n",
            ),
            ("vocabulary.md", "## 10:00:00\n\nIgnore these words\n\n"),
        ] {
            std::fs::write(root.join(name), text).unwrap();
        }
        use chrono::TimeZone;
        let at = Local.with_ymd_and_hms(2026, 10, 13, 10, 0, 0).unwrap();
        std::fs::write(
            root.join("speed.tsv"),
            format!(
                "{}\t60000\t120\n{}\t60000\t120\nbad line\n",
                at.timestamp_millis(),
                at.timestamp_millis() + 1000
            ),
        )
        .unwrap();
        let tally = Tally::read(&root, &root.join("speed.tsv"));
        assert_eq!(tally.total().wpm(), Some(120));
        let first = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        let second = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
        assert_eq!(tally.days.len(), 2);
        assert_eq!(
            tally.on(first),
            Count {
                words: 6,
                dictations: 3,
                timed_words: 240,
                spoken_ms: 120_000
            }
        );
        assert_eq!(
            tally.on(second),
            Count {
                words: 8,
                dictations: 1,
                ..Count::default()
            }
        );
        assert_eq!(
            tally.total(),
            Count {
                words: 14,
                dictations: 4,
                timed_words: 240,
                spoken_ms: 120_000
            }
        );
        assert_eq!(
            tally.since(second),
            Count {
                words: 8,
                dictations: 1,
                ..Count::default()
            }
        );
        assert!(tally.waiting.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

pub fn backfill(
    root: &Path,
    path: &Path,
    ledger: &Path,
    files: &[PathBuf],
    cutoff: i64,
) -> io::Result<()> {
    use std::io::Write;
    let mut entries = Vec::new();
    for (date, path) in crate::history::day_files(root) {
        if let crate::shadow::Text::Current(text) | crate::shadow::Text::Stale(text) =
            crate::shadow::read(&path)
        {
            for entry in crate::history::parse_day(date, &text) {
                if !entry.text.trim().is_empty() && entry.start_ms < cutoff {
                    entries.push((entry.start_ms, words(&entry.text), None::<u64>));
                }
            }
        }
    }
    entries.sort_by_key(|entry| entry.0);
    for event in crate::ledger::read(ledger) {
        if let crate::ledger::Event::Dictated {
            at,
            audio: Some(rel),
            ..
        } = event
        {
            if let (Ok(at), Ok(wav)) = (
                chrono::DateTime::parse_from_rfc3339(&at),
                hound::WavReader::open(ledger.join(rel)),
            ) {
                if wav.spec().sample_rate == 0 {
                    continue;
                }
                if let Some(entry) = entries.iter_mut().find(|entry| {
                    entry.2.is_none() && (-3000..=3000).contains(&(entry.0 - at.timestamp_millis()))
                }) {
                    entry.2 =
                        Some(u64::from(wav.duration()) * 1000 / u64::from(wav.spec().sample_rate));
                }
            }
        }
    }
    let mut rows = std::collections::BTreeMap::new();
    for file in files {
        if let Ok(text) = std::fs::read_to_string(file) {
            for line in text.lines() {
                let fields: Vec<_> = line.split('\t').collect();
                if fields.len() >= 4 {
                    if let Ok(ms) = fields[0].parse::<i64>() {
                        rows.entry((ms, fields[2].to_owned()))
                            .or_insert((fields[1].to_owned(), fields[3].to_owned()));
                    }
                }
            }
        }
    }
    let mut start = None;
    for ((ms, msg), (from, to)) in rows {
        if msg == "MainPressed" && to == "Dictating" {
            start = Some(ms);
        }
        if msg == "MainReleased" && from == "Dictating" {
            if let Some(start) = start.take() {
                let duration = ms - start;
                if (500..=600_000).contains(&duration) {
                    if let Some(entry) = entries
                        .iter_mut()
                        .find(|entry| entry.2.is_none() && (-1500..=8000).contains(&(entry.0 - ms)))
                    {
                        entry.2 = Some(duration as u64);
                    }
                }
            }
        }
    }
    (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path.with_extension("tsv.tmp"))?;
        for (at, words, duration) in &entries {
            if let Some(duration) = duration {
                writeln!(file, "{at}\t{duration}\t{words}")?;
            }
        }
        match std::fs::read(path) {
            Ok(current) => file.write_all(&current)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        file.sync_all()?;
        std::fs::rename(path.with_extension("tsv.tmp"), path)?;
        Ok(())
    })()
}

pub const TYPING_WPM: u64 = 40;
pub const LEVELS: [(&str, u64); 7] = [
    ("Letter", 1_000),
    ("Essay", 5_000),
    ("Novella", 25_000),
    ("Novel", 100_000),
    ("Epic", 250_000),
    ("Saga", 500_000),
    ("Million", 1_000_000),
];

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NextLevel {
    pub index: usize,
    pub name: &'static str,
    pub at: u64,
    pub to_go: u64,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Level {
    pub index: usize,
    pub name: Option<&'static str>,
    pub at: u64,
    pub next: Option<NextLevel>,
}
pub fn level(total: u64) -> Level {
    let index = LEVELS.iter().take_while(|(_, at)| total >= *at).count();
    Level {
        index,
        name: index.checked_sub(1).map(|i| LEVELS[i].0),
        at: index.checked_sub(1).map_or(0, |i| LEVELS[i].1),
        next: LEVELS.get(index).map(|&(name, at)| NextLevel {
            index: index + 1,
            name,
            at,
            to_go: at - total,
        }),
    }
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub history: bool,
    pub total: u64,
    pub since: Option<String>,
    pub wpm: Option<u64>,
    pub typing_wpm: u64,
    pub times_typing: Option<f64>,
    pub hours_typed: f64,
    pub hours_spoken: Option<f64>,
    pub hours_saved: Option<f64>,
    pub today: serde_json::Value,
    pub last7: serde_json::Value,
    pub best_day: Option<serde_json::Value>,
    pub fastest_day: Option<serde_json::Value>,
    pub active_days: usize,
    pub days: Vec<serde_json::Value>,
    pub level: Level,
    pub levels: Vec<serde_json::Value>,
    pub speed_levels: Vec<serde_json::Value>,
}
fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}
pub fn summary(tally: &Tally, today: NaiveDate) -> Summary {
    use serde_json::json;
    let total = tally.total();
    let wpm = total.wpm();
    let typed = total.words as f64 / TYPING_WPM as f64 / 60.0;
    let spoken = wpm
        .filter(|&n| n > 0)
        .map(|n| total.words as f64 / n as f64 / 60.0);
    let since = tally
        .days
        .iter()
        .find(|(_, c)| c.dictations > 0 || c.words > 0)
        .map(|(&d, _)| d);
    let mut days = Vec::new();
    if let Some(mut date) = since {
        while date <= today {
            let c = tally.on(date);
            days.push(json!({"date": date.to_string(), "words": c.words, "dictations": c.dictations, "wpm": c.wpm()}));
            let Some(next) = date.succ_opt() else { break };
            date = next;
        }
    }
    let last7 = tally
        .days
        .range((today - chrono::Days::new(6))..=today)
        .fold(Count::default(), |mut sum, (_, c)| {
            sum += *c;
            sum
        });
    let mut running = 0;
    let mut reached: [Option<String>; 7] = Default::default();
    for (&date, count) in &tally.days {
        running += count.words;
        for (i, &(_, at)) in LEVELS.iter().enumerate() {
            if running >= at && reached[i].is_none() {
                reached[i] = Some(date.to_string());
            }
        }
    }
    let best = tally.days.values().filter_map(Count::wpm).max();
    Summary {
        history: true,
        total: total.words,
        since: since.map(|d| d.to_string()),
        wpm,
        typing_wpm: TYPING_WPM,
        times_typing: wpm.map(|n| round1(n as f64 / 40.0)),
        hours_typed: round1(typed),
        hours_spoken: spoken.map(round1),
        hours_saved: spoken.map(|s| round1(typed - s)),
        today: json!({"date": today.to_string(), "words": tally.on(today).words, "wpm": tally.on(today).wpm()}),
        last7: json!({"words": last7.words, "perDay": (last7.words as f64 / 7.0).round() as u64, "wpm": last7.wpm()}),
        best_day: tally
            .days
            .iter()
            .filter(|(_, c)| c.words > 0)
            .max_by_key(|(_, c)| c.words)
            .map(|(d, c)| json!({"date":d.to_string(),"words":c.words})),
        fastest_day: tally
            .days
            .iter()
            .filter(|(_, c)| c.spoken_ms >= 300_000)
            .filter_map(|(d, c)| c.wpm().map(|w| (d, w)))
            .max_by_key(|(_, w)| *w)
            .map(|(d, w)| json!({"date":d.to_string(),"wpm":w})),
        active_days: tally.days.values().filter(|c| c.dictations > 0).count(),
        days,
        level: level(total.words),
        levels: LEVELS
            .iter()
            .enumerate()
            .map(|(i, (name, at))| json!({"index":i+1,"name":name,"at":at,"reached":reached[i]}))
            .collect(),
        speed_levels: [("Brisk", 2, 80), ("Fast", 3, 120), ("Flying", 4, 160)]
            .into_iter()
            .map(|(name, times, wpm)| {
                let days = tally
                    .days
                    .values()
                    .filter_map(Count::wpm)
                    .filter(|&w| w >= wpm)
                    .count();
                let mut value = json!({"name":name,"times":times,"wpm":wpm,"days":days});
                if days == 0 {
                    value["best"] = json!(best);
                }
                value
            })
            .collect(),
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn word_level_boundaries() {
        for (total, index, name, at, next) in [
            (999, 0, None, 0, Some(1_000)),
            (1_000, 1, Some("Letter"), 1_000, Some(5_000)),
            (99_999, 3, Some("Novella"), 25_000, Some(100_000)),
            (100_000, 4, Some("Novel"), 100_000, Some(250_000)),
            (1_000_000, 7, Some("Million"), 1_000_000, None),
        ] {
            let got = level(total);
            assert_eq!(
                (got.index, got.name, got.at, got.next.map(|n| n.at)),
                (index, name, at, next)
            );
        }
    }

    #[test]
    fn summary_counts_dates_speed_and_estimates() {
        let date = |day| NaiveDate::from_ymd_opt(2026, 11, day).unwrap();
        let tally = Tally {
            days: BTreeMap::from([
                (
                    date(1),
                    Count {
                        words: 900,
                        dictations: 3,
                        timed_words: 600,
                        spoken_ms: 300_000,
                    },
                ),
                (
                    date(3),
                    Count {
                        words: 4200,
                        dictations: 2,
                        timed_words: 200,
                        spoken_ms: 60_000,
                    },
                ),
                (
                    date(4),
                    Count {
                        words: 100,
                        dictations: 1,
                        timed_words: 400,
                        spoken_ms: 300_000,
                    },
                ),
            ]),
            waiting: vec![],
        };
        let got = serde_json::to_value(summary(&tally, date(4))).unwrap();
        assert_eq!(got["total"], 5200);
        assert_eq!(got["wpm"], 109);
        assert_eq!(got["hoursTyped"], 2.2);
        assert_eq!(got["hoursSpoken"], 0.8);
        assert_eq!(got["hoursSaved"], 1.4);
        assert_eq!(got["timesTyping"], 2.7);
        assert_eq!(
            got["level"],
            json!({"index":2,"name":"Essay","at":5000,"next":{"index":3,"name":"Novella","at":25000,"toGo":19800}})
        );
        assert_eq!(got["levels"][0]["reached"], "2026-11-03");
        assert_eq!(got["levels"][1]["reached"], "2026-11-03");
        assert_eq!(got["levels"][2]["reached"], json!(null));
        assert_eq!(
            got["speedLevels"],
            json!([
                {"name":"Brisk","times":2,"wpm":80,"days":3},
                {"name":"Fast","times":3,"wpm":120,"days":2},
                {"name":"Flying","times":4,"wpm":160,"days":1}
            ])
        );
        assert_eq!(got["fastestDay"], json!({"date":"2026-11-01","wpm":120}));
        assert_eq!(
            got["days"][1],
            json!({"date":"2026-11-02","words":0,"dictations":0,"wpm":null})
        );
        assert_eq!(got["activeDays"], 3);
        let empty = serde_json::to_value(summary(&Tally::default(), date(4))).unwrap();
        assert_eq!(empty["wpm"], json!(null));
        assert_eq!(empty["hoursSaved"], json!(null));
        assert_eq!(empty["fastestDay"], json!(null));
    }
}

#[cfg(test)]
mod backfill_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn backfill_cutoff_and_atomic_merge_preserve_sources() {
        let root = std::env::temp_dir().join(format!(
            "see-backfill-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let day = root.join("2026-11-01.md");
        let text = "## 12:00:02\n\none two three\n\n## 12:00:03\n\nlater words\n\n";
        std::fs::write(&day, text).unwrap();
        let trail = root.join("events.tsv");
        let at = Local
            .with_ymd_and_hms(2026, 11, 1, 12, 0, 0)
            .unwrap()
            .timestamp_millis();
        let rows = format!(
            "{at}\tIdle\tMainPressed\tDictating\n{}\tDictating\tMainReleased\tTranscribing\n",
            at + 2000
        );
        std::fs::write(&trail, &rows).unwrap();
        let speed = root.join("speed.tsv");
        std::fs::write(&speed, "999\t60000\t120\n").unwrap();
        backfill(
            &root,
            &speed,
            &root.join("ledger"),
            std::slice::from_ref(&trail),
            at + 3000,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&speed).unwrap(),
            format!("{}\t2000\t3\n999\t60000\t120\n", at + 2000)
        );
        assert_eq!(std::fs::read_to_string(day).unwrap(), text);
        assert_eq!(std::fs::read_to_string(trail).unwrap(), rows);
        std::fs::remove_dir_all(root).unwrap();
    }
}
