//! What the user's own corrections teach: which spellings to boost, and which
//! phrases the model keeps hearing in their place.
//!
//! Pure functions over the ledger's events, so learning is replayable and the
//! same log always yields the same decisions.

use crate::ledger::{Event, Id, Source};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub struct Swap {
    pub heard: String,
    pub wrote: String,
    pub opens_sentence: bool,
}

fn strip(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}
fn form(token: &str) -> String {
    strip(token).to_lowercase()
}

pub(crate) fn alignment(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let af: Vec<_> = a.iter().map(|s| form(s)).collect();
    let bf: Vec<_> = b.iter().map(|s| form(s)).collect();
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            dp[i][j] = if af[i] == bf[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut anchors = Vec::new();
    while i < a.len() && j < b.len() {
        if af[i] == bf[j] {
            anchors.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    anchors
}

pub fn swaps(pasted: &str, edited: &str) -> Vec<Swap> {
    let a: Vec<_> = pasted.split_whitespace().collect();
    let b: Vec<_> = edited.split_whitespace().collect();
    if a.len() > 400 || b.len() > 400 {
        return Vec::new();
    }
    let anchors = alignment(&a, &b);
    let mut changed = 0usize;
    let mut out = Vec::new();
    let (mut ai, mut bj) = (0, 0);
    for (x, y) in anchors
        .iter()
        .copied()
        .chain(std::iter::once((a.len(), b.len())))
    {
        let (left, right) = (x - ai, y - bj);
        changed += if left > 0 && right > 0 {
            1
        } else {
            left.max(right)
        };
        if (1..=4).contains(&left) && (1..=3).contains(&right) {
            let heard = a[ai..x]
                .iter()
                .map(|s| strip(s))
                .collect::<Vec<_>>()
                .join(" ");
            let wrote = b[bj..y]
                .iter()
                .map(|s| strip(s))
                .collect::<Vec<_>>()
                .join(" ");
            if !heard.is_empty() && !wrote.is_empty() {
                out.push(Swap {
                    heard,
                    wrote,
                    opens_sentence: bj == 0 || b[bj - 1].ends_with(['.', '!', '?']),
                });
            }
        }
        if x < a.len() {
            let old = strip(a[x]);
            let new = strip(b[y]);
            if old != new && old.to_lowercase() == new.to_lowercase() {
                changed += 1;
                out.push(Swap {
                    heard: old.into(),
                    wrote: new.into(),
                    opens_sentence: y == 0 || b[y - 1].ends_with(['.', '!', '?']),
                });
            }
        }
        ai = x + 1;
        bj = y + 1;
    }
    if changed > 2.max(a.len() / 2) {
        Vec::new()
    } else {
        out
    }
}

/// The words a model could plausibly write unprompted. macOS ships the list,
/// so the app ships none. Loaded on first use, because it costs about ten
/// megabytes and is only needed once the user has corrected something.
pub struct Words(std::sync::OnceLock<HashSet<String>>);

impl Words {
    pub fn system() -> Words {
        Words(std::sync::OnceLock::new())
    }

    #[cfg(test)]
    pub fn from_list(words: &[&str]) -> Words {
        Words(std::sync::OnceLock::from(
            words
                .iter()
                .map(|s| s.to_lowercase())
                .collect::<HashSet<_>>(),
        ))
    }

    fn set(&self) -> &HashSet<String> {
        self.0.get_or_init(|| {
            std::fs::read_to_string("/usr/share/dict/words")
                .unwrap_or_default()
                .lines()
                .map(str::to_lowercase)
                .collect()
        })
    }

    /// Inflections count: the list holds base forms only, so "sketched" would
    /// otherwise look like a term the user taught.
    pub fn knows(&self, word: &str) -> bool {
        let set = self.set();
        let lower = word.to_lowercase();
        if set.contains(&lower) {
            return true;
        }
        for suffix in ["'s", "ers", "ing", "ed", "es", "ly", "er", "s", "d"] {
            if let Some(stem) = lower.strip_suffix(suffix) {
                if set.contains(stem) {
                    return true;
                }
                if let Some((last, prior)) = stem.chars().next_back().zip(stem.chars().rev().nth(1))
                {
                    if last == prior
                        && !"aeiou".contains(last)
                        && set.contains(&stem[..stem.len() - last.len_utf8()])
                    {
                        return true;
                    }
                }
                if matches!(suffix, "ed" | "ing") && set.contains(&format!("{stem}e")) {
                    return true;
                }
            }
        }
        false
    }
}

pub fn learnable(swap: &Swap, words: &Words) -> bool {
    if swap.heard.to_lowercase() == swap.wrote.to_lowercase()
        && swap.heard.chars().any(char::is_uppercase)
        && !swap.wrote.chars().any(char::is_uppercase)
    {
        return false;
    }
    for (index, word) in swap.wrote.split_whitespace().enumerate() {
        let letters: Vec<_> = word.chars().filter(|c| c.is_alphabetic()).collect();
        if letters.is_empty() {
            continue;
        }
        if letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase()) {
            return true;
        }
        if word.chars().skip(1).any(|c| c.is_uppercase()) {
            return true;
        }
        if word
            .chars()
            .skip(1)
            .take(word.chars().count().saturating_sub(2))
            .any(|c| "._-/".contains(c))
        {
            return true;
        }
        if word.chars().any(|c| c.is_alphabetic()) && word.chars().any(|c| c.is_numeric()) {
            return true;
        }
        if word.chars().next().is_some_and(char::is_uppercase)
            && !(index == 0 && swap.opens_sentence)
        {
            return true;
        }
        if !words.knows(word) {
            return true;
        }
    }
    false
}

#[derive(Clone, Debug, PartialEq)]
pub struct Promotion {
    pub term: String,
    pub heard: Option<String>,
    pub from: Vec<Id>,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Waiting {
    pub term: String,
    pub heard: Vec<String>,
    pub from: Vec<Id>,
}

/// One dictation's evidence for a term: the id, what the model heard instead,
/// and whether the user typed the fix into the Fix dialog.
struct Sighting {
    id: Id,
    heard: String,
    source: Source,
}

impl Sighting {
    /// A fix the user typed on purpose is worth a second sighting.
    fn weight(&self) -> usize {
        match self.source {
            Source::Manual => 2,
            Source::ReadBack => 1,
        }
    }
}

fn weight<'a>(sightings: impl IntoIterator<Item = &'a Sighting>) -> usize {
    sightings.into_iter().map(Sighting::weight).sum()
}

/// What the event log already decided about one term.
#[derive(Default)]
struct Decided {
    term_learned: bool,
    rules: HashSet<String>,
    forgotten: bool,
}

fn decisions(events: &[Event]) -> HashMap<&str, Decided> {
    let mut decided: HashMap<&str, Decided> = HashMap::new();
    for event in events {
        match event {
            Event::Learned { term, heard, .. } => {
                let entry = decided.entry(term.as_str()).or_default();
                entry.term_learned = true;
                if let Some(heard) = heard {
                    entry.rules.insert(heard.clone());
                }
            }
            Event::Forgotten { term, .. } => {
                decided.entry(term.as_str()).or_default().forgotten = true
            }
            _ => {}
        }
    }
    decided
}

/// Every learnable swap in the latest correction of each dictation, grouped
/// by the term the user wrote, in the order the terms were first seen.
fn candidates(events: &[Event], words: &Words) -> Vec<(String, Vec<Sighting>)> {
    let mut dictated = HashMap::new();
    let mut corrected = BTreeMap::new();
    for event in events {
        match event {
            Event::Dictated { id, text, .. } => {
                dictated.insert(id, text.as_str());
            }
            Event::Corrected {
                id, text, source, ..
            } => {
                corrected.insert(id, (text.as_str(), *source));
            }
            _ => {}
        }
    }
    let mut terms: Vec<(String, Vec<Sighting>)> = Vec::new();
    for (id, (edited, source)) in corrected {
        let Some(pasted) = dictated.get(id) else {
            continue;
        };
        for swap in swaps(pasted, edited) {
            if !learnable(&swap, words) {
                continue;
            }
            let index = match terms.iter().position(|(term, _)| *term == swap.wrote) {
                Some(index) => index,
                None => {
                    terms.push((swap.wrote.clone(), Vec::new()));
                    terms.len() - 1
                }
            };
            let sightings = &mut terms[index].1;
            if !sightings.iter().any(|seen| seen.id == *id) {
                sightings.push(Sighting {
                    id: id.clone(),
                    heard: swap.heard.to_lowercase(),
                    source,
                });
            }
        }
    }
    terms
}

/// Terms and rules the evidence now supports and the log has not yet decided.
///
/// A rule can attach to a term already in the vocabulary: that is the case
/// boosting cannot fix, a term the model keeps hearing as an ordinary word.
pub fn promotions(events: &[Event], known: &[String], words: &Words) -> Vec<Promotion> {
    let decided = decisions(events);
    let mut out = Vec::new();
    for (term, sightings) in candidates(events, words) {
        let done = decided.get(term.as_str());
        if done.is_some_and(|done| done.forgotten) {
            continue;
        }
        let mut heard_forms: Vec<&str> = Vec::new();
        for sighting in &sightings {
            if !heard_forms.contains(&sighting.heard.as_str()) {
                heard_forms.push(&sighting.heard);
            }
        }
        let mut rules = Vec::new();
        for heard in heard_forms {
            let same: Vec<_> = sightings.iter().filter(|s| s.heard == heard).collect();
            let learned = done.is_some_and(|done| done.rules.contains(heard));
            let ordinary = heard.split_whitespace().all(|word| words.knows(word));
            let supported = if ordinary {
                same.iter().map(|s| &s.id).collect::<HashSet<_>>().len() >= 3
            } else {
                weight(same.iter().copied()) >= 2
            };
            if supported && !learned {
                rules.push(Promotion {
                    term: term.clone(),
                    heard: Some(heard.to_owned()),
                    from: same.iter().map(|s| s.id.clone()).collect(),
                });
            }
        }
        let term_settled = known.contains(&term) || done.is_some_and(|done| done.term_learned);
        if rules.is_empty() && !term_settled && weight(&sightings) >= 2 {
            out.push(Promotion {
                term,
                heard: None,
                from: sightings.into_iter().map(|s| s.id).collect(),
            });
        } else {
            out.extend(rules);
        }
    }
    out
}

/// Terms seen once, newest first: the review queue the menu offers.
pub fn waiting(events: &[Event], known: &[String], words: &Words) -> Vec<Waiting> {
    let decided = decisions(events);
    let mut queue: Vec<Waiting> = candidates(events, words)
        .into_iter()
        .filter(|(term, sightings)| {
            weight(sightings) == 1 && !known.contains(term) && !decided.contains_key(term.as_str())
        })
        .map(|(term, sightings)| Waiting {
            term,
            heard: sightings.iter().map(|s| s.heard.clone()).collect(),
            from: sightings.into_iter().map(|s| s.id).collect(),
        })
        .collect();
    queue.sort_by(|a, b| b.from.cmp(&a.from));
    queue
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(id: &str, text: &str) -> Event {
        Event::Dictated {
            id: Id::test(id),
            at: String::new(),
            text: text.into(),
            audio: None,
        }
    }
    fn correction(id: &str, text: &str, source: Source) -> Event {
        Event::Corrected {
            id: Id::test(id),
            at: String::new(),
            text: text.into(),
            source,
        }
    }
    #[test]
    fn swaps_find_terms_without_rewrites() {
        assert_eq!(
            swaps(
                "Ask Codex to open a pull request on Git hub.",
                "Ask Codex to open a pull request on GitHub."
            ),
            vec![Swap {
                heard: "Git hub".into(),
                wrote: "GitHub".into(),
                opens_sentence: false
            }]
        );
        let found = swaps(
            "we moved from superbase to work os",
            "we moved from Supabase to WorkOS",
        );
        assert_eq!(
            found
                .iter()
                .map(|s| (s.heard.as_str(), s.wrote.as_str()))
                .collect::<Vec<_>>(),
            [("superbase", "Supabase"), ("work os", "WorkOS")]
        );
        assert!(swaps(
            "this is all wrong now",
            "completely different words here today"
        )
        .is_empty());
        assert!(swaps("Hello, world!", "Hello world.").is_empty());
        assert!(swaps("hello world", "hello world. Another sentence.").is_empty());
    }
    #[test]
    fn filter_and_inflections() {
        let words = Words::from_list(&[
            "sketch", "plan", "class", "run", "too", "to", "deploy", "dev", "paris",
        ]);
        for word in ["planned", "sketched", "classes", "running"] {
            assert!(words.knows(word), "{word}");
        }
        for (heard, wrote, opens, yes) in [
            ("superbase", "Supabase", false, true),
            ("work os", "WorkOS", false, true),
            ("test flight", "testflight", false, true),
            ("claude md", "CLAUDE.md", false, true),
            ("sketch", "sketched", false, false),
            ("to", "too", false, false),
            ("deploy", "Deploy", true, false),
            ("Dev", "dev", false, false),
            ("paris", "Paris", false, true),
        ] {
            assert_eq!(
                learnable(
                    &Swap {
                        heard: heard.into(),
                        wrote: wrote.into(),
                        opens_sentence: opens
                    },
                    &words
                ),
                yes,
                "{wrote}"
            );
        }
    }
    #[test]
    fn promotion_counts_distinct_dictations() {
        let words = Words::from_list(&[]);
        let mut events = vec![
            event("1", "superbase"),
            correction("1", "Supabase", Source::ReadBack),
        ];
        assert!(promotions(&events, &[], &words).is_empty());
        assert_eq!(waiting(&events, &[], &words)[0].term, "Supabase");
        events.extend([
            event("2", "superbase"),
            correction("2", "Supabase", Source::ReadBack),
        ]);
        let promoted = promotions(&events, &[], &words);
        assert_eq!(
            promoted,
            vec![Promotion {
                term: "Supabase".into(),
                heard: Some("superbase".into()),
                from: vec![Id::test("1"), Id::test("2")]
            }]
        );
        assert_eq!(promotions(&events, &[], &words), promoted);
        assert_eq!(
            promotions(&events, &["Supabase".into()], &words),
            promoted,
            "a known term still earns the rule for what the model hears instead"
        );
        let differently_heard = vec![
            event("4", "super base"),
            correction("4", "Supabase", Source::ReadBack),
            event("5", "superbass"),
            correction("5", "Supabase", Source::ReadBack),
        ];
        assert!(promotions(&differently_heard, &["Supabase".into()], &words).is_empty());
        events.push(Event::Forgotten {
            at: String::new(),
            term: "Supabase".into(),
        });
        assert!(promotions(&events, &[], &words).is_empty());
        let manual = vec![
            event("3", "superbase"),
            correction("3", "Supabase", Source::Manual),
        ];
        assert_eq!(
            promotions(&manual, &[], &words)[0].heard,
            Some("superbase".into())
        );
    }

    #[test]
    fn a_known_term_the_model_keeps_mishearing_earns_a_rule() {
        let words = Words::from_list(&["potato", "mode", "use"]);
        let events = vec![
            event("1", "use potato mode"),
            correction("1", "use poteto mode", Source::ReadBack),
            event("2", "potato mode"),
            correction("2", "poteto mode", Source::ReadBack),
            event("3", "potato mode"),
            correction("3", "poteto mode", Source::ReadBack),
        ];
        assert_eq!(
            promotions(&events, &["poteto".into()], &words),
            vec![Promotion {
                term: "poteto".into(),
                heard: Some("potato".into()),
                from: vec![Id::test("1"), Id::test("2"), Id::test("3")]
            }]
        );
    }

    #[test]
    fn ordinary_heard_words_need_three_dictations() {
        let words = Words::from_list(&["we", "use", "potato"]);
        let mut events = vec![
            event("1", "we use potato"),
            correction("1", "we use Poteto", Source::Manual),
        ];
        assert_eq!(promotions(&events, &[], &words)[0].heard, None);
        for id in ["2", "3"] {
            events.push(event(id, "we use potato"));
            events.push(correction(id, "we use Poteto", Source::ReadBack));
        }
        assert_eq!(
            promotions(&events, &[], &words)[0].heard.as_deref(),
            Some("potato")
        );
    }

    #[test]
    fn numbers_are_not_terms() {
        let words = Words::from_list(&["in"]);
        let swap = Swap {
            heard: "20".into(),
            wrote: "2026".into(),
            opens_sentence: false,
        };
        assert!(!learnable(&swap, &words));
    }

    #[test]
    fn waiting_names_the_dictation_it_came_from() {
        let words = Words::from_list(&[]);
        let events = vec![
            event("1", "superbase"),
            correction("1", "Supabase", Source::ReadBack),
        ];
        assert_eq!(
            waiting(&events, &[], &words),
            vec![Waiting {
                term: "Supabase".into(),
                heard: vec!["superbase".into()],
                from: vec![Id::test("1")]
            }]
        );
    }
}
