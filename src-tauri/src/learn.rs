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

pub fn learned_swaps(pasted: &str, edited: &str, words: &Words) -> Vec<Swap> {
    swaps(pasted, edited)
        .into_iter()
        .flat_map(|swap| split_names_from_plain_words(swap, words))
        .filter(|swap| learnable(swap, words))
        .collect()
}

fn split_names_from_plain_words(swap: Swap, words: &Words) -> Vec<Swap> {
    let wrote: Vec<&str> = swap.wrote.split_whitespace().collect();
    let heard: Vec<&str> = swap.heard.split_whitespace().collect();
    let plain = |index: usize, word: &str| {
        !learnable(
            &Swap {
                heard: String::new(),
                wrote: word.to_owned(),
                opens_sentence: index == 0 && swap.opens_sentence,
            },
            words,
        )
    };
    struct Run {
        start: usize,
        end: usize,
        plain: bool,
    }
    let mut runs: Vec<Run> = Vec::new();
    for (index, word) in wrote.iter().enumerate() {
        let is_plain = plain(index, word);
        match runs.last_mut() {
            Some(run) if run.plain == is_plain => run.end = index + 1,
            _ => runs.push(Run {
                start: index,
                end: index + 1,
                plain: is_plain,
            }),
        }
    }
    if runs.len() < 2 {
        return vec![swap];
    }
    if heard.len() < runs.len() {
        return Vec::new();
    }
    let spoken = |words: &[&str]| -> Vec<char> {
        const DIGITS: [&str; 10] = [
            "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
        ];
        let mut out = Vec::new();
        for c in words.concat().chars().flat_map(char::to_lowercase) {
            match c.to_digit(10) {
                Some(d) => out.extend(DIGITS[d as usize].chars()),
                None if c.is_alphabetic() => out.push(c),
                None => {}
            }
        }
        out
    };
    let mut best: Option<(usize, Vec<usize>)> = None;
    let mut cuts = vec![0usize; runs.len() + 1];
    cuts[runs.len()] = heard.len();
    fn each_partition(
        cuts: &mut Vec<usize>,
        at: usize,
        total: usize,
        visit: &mut dyn FnMut(&[usize]),
    ) {
        if at == cuts.len() - 1 {
            visit(cuts);
            return;
        }
        let remaining = cuts.len() - 1 - at;
        for cut in cuts[at - 1] + 1..=total - remaining {
            cuts[at] = cut;
            each_partition(cuts, at + 1, total, visit);
        }
    }
    each_partition(&mut cuts, 1, heard.len(), &mut |cuts| {
        let cost = runs
            .iter()
            .enumerate()
            .map(|(i, &Run { start, end, .. })| {
                distance(
                    &spoken(&heard[cuts[i]..cuts[i + 1]]),
                    &spoken(&wrote[start..end]),
                )
            })
            .sum();
        if best.as_ref().is_none_or(|(least, _)| cost < *least) {
            best = Some((cost, cuts.to_vec()));
        }
    });
    let Some((_, cuts)) = best else {
        return vec![swap];
    };
    runs.iter()
        .enumerate()
        .map(|(i, &Run { start, end, .. })| Swap {
            heard: heard[cuts[i]..cuts[i + 1]].join(" "),
            wrote: wrote[start..end].join(" "),
            opens_sentence: start == 0 && swap.opens_sentence,
        })
        .collect()
}

fn distance(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, y) in b.iter().enumerate() {
            current.push(
                (previous[j + 1] + 1)
                    .min(current[j] + 1)
                    .min(previous[j] + usize::from(x != y)),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

/// A single owner thread keeps NSSpellChecker calls serial.
pub struct Words(WordSource);

enum WordSource {
    Checker(std::sync::mpsc::Sender<Ask>),
    #[cfg(test)]
    List(HashSet<String>),
}

struct Ask {
    word: String,
    reply: std::sync::mpsc::Sender<bool>,
    deadline: std::time::Instant,
}

impl Words {
    pub fn shared() -> &'static Words {
        static SHARED: std::sync::OnceLock<Words> = std::sync::OnceLock::new();
        SHARED.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Ask>();
            // NSSpellChecker is not safe to call concurrently: eight threads
            // got 16 wrong answers in 4,000 on 2026-11-02.
            crate::qos::spawn("see-words", crate::qos::Class::Upkeep, move || {
                let languages = proven_languages();
                let mut memo = HashMap::new();
                while let Ok(ask) = rx.recv() {
                    let answer = memoized_word(&mut memo, &ask.word, || {
                        if languages.is_empty() || std::time::Instant::now() >= ask.deadline {
                            return None;
                        }
                        let mut known = false;
                        for language in &languages {
                            known |= check_word(&ask.word, language)?;
                        }
                        (std::time::Instant::now() < ask.deadline).then_some(known)
                    });
                    let _ = ask.reply.send(answer);
                }
            });
            Words(WordSource::Checker(tx))
        })
    }

    #[cfg(test)]
    pub fn from_list(words: &[&str]) -> Words {
        Words(WordSource::List(
            words.iter().map(|s| (*s).to_owned()).collect(),
        ))
    }

    pub fn knows(&self, word: &str) -> bool {
        self.ask(word, std::time::Duration::from_secs(2))
    }

    pub fn knows_quickly(&self, word: &str) -> bool {
        self.ask(word, std::time::Duration::from_millis(50))
    }

    fn ask(&self, word: &str, limit: std::time::Duration) -> bool {
        match &self.0 {
            #[cfg(test)]
            WordSource::List(set) => set.contains(word) || set.contains(&word.to_lowercase()),
            WordSource::Checker(tx) => {
                let (reply, rx) = std::sync::mpsc::channel();
                if tx
                    .send(Ask {
                        word: word.into(),
                        reply,
                        deadline: std::time::Instant::now() + limit,
                    })
                    .is_err()
                {
                    return true;
                }
                rx.recv_timeout(limit).unwrap_or(true)
            }
        }
    }
}

/// "Unknown" is asked twice before it sticks, because WebKit may use the same
/// checker from the main thread for the Library's fix field.
fn memoized_word(
    memo: &mut HashMap<String, bool>,
    word: &str,
    mut check: impl FnMut() -> Option<bool>,
) -> bool {
    if let Some(known) = memo.get(word) {
        return *known;
    }
    let Some(mut known) = check() else {
        return true;
    };
    if !known {
        let Some(second) = check() else {
            return true;
        };
        known = second;
    }
    memo.insert(word.into(), known);
    known
}

#[repr(C)]
struct SpellRange {
    location: usize,
    length: usize,
}
unsafe impl objc2::Encode for SpellRange {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Struct("_NSRange", &[usize::ENCODING, usize::ENCODING]);
}

fn check_word(word: &str, language: &str) -> Option<bool> {
    use objc2::{
        msg_send,
        runtime::{AnyClass, AnyObject},
    };
    objc2::rc::autoreleasepool(|_| unsafe {
        let class = AnyClass::get(c"NSSpellChecker")?;
        let string = AnyClass::get(c"NSString")?;
        let checker: *mut AnyObject = msg_send![class, sharedSpellChecker];
        if checker.is_null() {
            return None;
        }
        let word = std::ffi::CString::new(word).ok()?;
        let language = std::ffi::CString::new(language).ok()?;
        let word: *mut AnyObject = msg_send![string, stringWithUTF8String: word.as_ptr()];
        let language: *mut AnyObject = msg_send![string, stringWithUTF8String: language.as_ptr()];
        if word.is_null() || language.is_null() {
            return None;
        }
        let count: *mut isize = std::ptr::null_mut();
        let range: SpellRange = msg_send![checker, checkSpellingOfString: word,
            startingAt: 0isize, language: language, wrap: false,
            inSpellDocumentWithTag: 0isize, wordCount: count];
        Some(range.length == 0)
    })
}

/// Some languages `NSSpellChecker` lists pass nonsense as correctly spelled.
fn proven_languages() -> Vec<String> {
    use objc2::{
        msg_send,
        runtime::{AnyClass, AnyObject},
    };
    let mut languages = objc2::rc::autoreleasepool(|_| unsafe {
        let mut languages = vec!["en".to_owned()];
        if let Some(locale) = AnyClass::get(c"NSLocale") {
            let preferred: *mut AnyObject = msg_send![locale, preferredLanguages];
            if !preferred.is_null() {
                let count: usize = msg_send![preferred, count];
                for i in 0..count {
                    let language: *mut AnyObject = msg_send![preferred, objectAtIndex: i];
                    let utf8: *const std::ffi::c_char = msg_send![language, UTF8String];
                    if !utf8.is_null() {
                        let value = std::ffi::CStr::from_ptr(utf8).to_string_lossy();
                        let base = value
                            .split(['-', '_'])
                            .next()
                            .unwrap_or_default()
                            .to_owned();
                        if !languages.contains(&base) {
                            languages.push(base);
                        }
                    }
                }
            }
        }
        languages
    });
    languages.retain(|language| {
        check_word("zzqxvkj", language) == Some(false)
            && (check_word("a", language) == Some(true) || check_word("i", language) == Some(true))
    });
    languages
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

/// Names and identifiers in context text, the most frequent first.
///
/// Stricter than the correction test, because nobody confirmed these words:
/// a capital that opens a line or sentence, a dictionary word, and a plain
/// lowercase word all stay out. A label like "Settings" or a description
/// like "Comparing prices" would otherwise be boosted like a name. So does a
/// dictionary word capitalized mid-sentence: boosted from a quoted "Yeah.",
/// the decoder wrote "Yeah. Yeah." into the silence before every dictation.
pub fn context_terms(text: &str, words: &Words) -> Vec<String> {
    let mut found: Vec<(String, usize)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for line in text.lines() {
        // A word alone on its line is a label or a name, not a sentence.
        let mut opens = line.split_whitespace().nth(1).is_some();
        for part in line.split_whitespace() {
            // "Larsten's" counts as Larsten, and "I'll" as I.
            let token = strip(strip(part).split(['\'', '’']).next().unwrap_or_default());
            if token.len() <= 64 && names_something(token, opens, words) {
                match seen.get(&token.to_lowercase()) {
                    Some(&index) => found[index].1 += 1,
                    None => {
                        seen.insert(token.to_lowercase(), found.len());
                        found.push((token.to_owned(), 1));
                    }
                }
            }
            opens = part.ends_with(['.', '!', '?', ':']);
        }
    }
    // Stable, so equally frequent terms keep the order they were seen in.
    found.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    found.into_iter().map(|(term, _)| term).collect()
}

/// Short forms a window shows everywhere, and spoken words, that the word
/// list lacks.
const ORDINARY: &[&str] = &[
    "jan", "feb", "mar", "apr", "jun", "jul", "aug", "sep", "sept", "oct", "nov", "dec", "mon",
    "tue", "wed", "thu", "fri", "sat", "sun", "am", "pm", "kb", "mb", "gb", "tb", "ok", "okay",
    "yup", "hmm", "gonna", "wanna", "gotta", "uh", "nah",
];

fn names_something(token: &str, opens: bool, words: &Words) -> bool {
    let letters = token.chars().filter(|c| c.is_alphabetic()).count();
    if letters < 2 || ORDINARY.contains(&token.to_lowercase().as_str()) {
        return false;
    }
    let inner: String = token
        .chars()
        .skip(1)
        .take(token.chars().count() - 2)
        .collect();
    // A path or branch name is read, never said: "apps/web/sandbox/drawers",
    // "t3code/project-lat". Only a short alternation like "A/B" is spoken.
    if inner.contains('/') {
        return token.split('/').all(|part| part.chars().count() <= 2);
    }
    if token.chars().any(char::is_numeric) {
        return true;
    }
    if inner.contains(['.', '_']) {
        return true;
    }
    if inner.contains('-') {
        // T3 Code shows a long branch name as two elements, and the second
        // opens mid-word: "est-status", "nding-page". A name opens with a
        // word, a capital or a digit.
        let opens_like_a_word = token.split('-').next().is_some_and(|part| {
            words.knows_quickly(part)
                || part
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_uppercase() || c.is_numeric())
        });
        return opens_like_a_word
            && token
                .split('-')
                .any(|part| !part.is_empty() && !words.knows_quickly(part));
    }
    let upper = token.chars().filter(|c| c.is_uppercase()).count();
    let lower = token.chars().filter(|c| c.is_lowercase()).count();
    let capital_inside = token.chars().skip(1).any(char::is_uppercase);
    if capital_inside && lower > 0 {
        return true;
    }
    if upper == 0 {
        return false;
    }
    if lower == 0 {
        return !words.knows_quickly(token);
    }
    !opens && !words.knows_quickly(token)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Promotion {
    pub term: String,
    pub heard: Option<String>,
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

pub fn forgotten(events: &[Event], term: &str, heard: Option<&str>) -> bool {
    for event in events.iter().rev() {
        match event {
            Event::Learned {
                term: t, heard: h, ..
            } if crate::vocabulary::same(t, term) && same_heard(h.as_deref(), heard) => {
                return false
            }
            Event::Forgotten {
                term: t, heard: h, ..
            } if crate::vocabulary::same(t, term)
                && (h.is_none() || same_heard(h.as_deref(), heard)) =>
            {
                return true
            }
            _ => {}
        }
    }
    false
}

fn same_heard(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => crate::vocabulary::same(a, b),
        _ => false,
    }
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
        for swap in learned_swaps(pasted, edited, words) {
            let index = match terms
                .iter()
                .position(|(term, _)| crate::vocabulary::same(term, &swap.wrote))
            {
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

pub fn promotions(
    events: &[Event],
    known: &[crate::vocabulary::Entry],
    words: &Words,
) -> Vec<Promotion> {
    let mut out = Vec::new();
    for (term, sightings) in candidates(events, words) {
        let settled = |heard: Option<&str>| {
            known.iter().any(|e| {
                crate::vocabulary::same(&e.term, &term)
                    && (heard.is_none() || same_heard(e.heard.as_deref(), heard))
            }) || events.iter().any(|e| {
                matches!(e, Event::Learned { term: t, heard: h, .. }
                    if crate::vocabulary::same(t, &term)
                        && (heard.is_none() || same_heard(h.as_deref(), heard)))
            }) || forgotten(events, &term, heard)
        };
        if !settled(None) {
            out.push(Promotion {
                term: term.clone(),
                heard: None,
                from: sightings.iter().map(|s| s.id.clone()).collect(),
            });
        }
        let mut forms = HashSet::new();
        for sighting in &sightings {
            let heard = sighting.heard.as_str();
            if !forms.insert(heard) || settled(Some(heard)) {
                continue;
            }
            let count = heard.split_whitespace().count();
            let safe =
                count >= 2 || (count == 1 && heard.chars().count() >= 2 && !words.knows(heard));
            let evidence: Vec<_> = sightings.iter().filter(|s| s.heard == heard).collect();
            if safe && weight(evidence.iter().copied()) >= 2 {
                out.push(Promotion {
                    term: term.clone(),
                    heard: Some(heard.into()),
                    from: evidence.iter().map(|s| s.id.clone()).collect(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixes(heard: &str, source: Source, count: usize) -> Vec<Event> {
        (0..count)
            .flat_map(|n| {
                [
                    Event::Dictated {
                        id: Id::test(&n.to_string()),
                        at: String::new(),
                        text: heard.into(),
                        audio: None,
                    },
                    Event::Corrected {
                        id: Id::test(&n.to_string()),
                        at: String::new(),
                        text: "Zentrix".into(),
                        source,
                    },
                ]
            })
            .collect()
    }
    fn rows(events: &[Event], known: &[crate::vocabulary::Entry]) -> Vec<(String, Option<String>)> {
        promotions(events, known, &Words::from_list(&["river"]))
            .into_iter()
            .map(|p| (p.term, p.heard))
            .collect()
    }

    #[test]
    fn checker_failure_is_known_and_not_cached() {
        let mut memo = HashMap::new();
        assert!(memoized_word(&mut memo, "name", || None));
        assert!(memo.is_empty());
        let mut answers = [Some(false), None].into_iter();
        assert!(memoized_word(&mut memo, "name", || answers.next().unwrap()));
        assert!(memo.is_empty());
        assert!(!memoized_word(&mut memo, "name", || Some(false)));
        assert_eq!(memo.get("name"), Some(&false));
    }

    #[test]
    #[ignore]
    fn real_checker_serializes_concurrent_callers() {
        let words = Words::shared();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(move || {
                    for _ in 0..100 {
                        for (word, known) in [
                            ("tugga", true),
                            ("skool", false),
                            ("london", false),
                            ("London", true),
                        ] {
                            assert_eq!(words.knows(word), known, "{word}");
                        }
                    }
                });
            }
        });
    }

    #[test]
    fn first_fix_only_boosts_and_second_fix_can_replace() {
        assert_eq!(
            rows(&fixes("zentrixx", Source::ReadBack, 1), &[]),
            [("Zentrix".into(), None)]
        );
        assert_eq!(
            rows(&fixes("zentrixx", Source::ReadBack, 2), &[]),
            [
                ("Zentrix".into(), None),
                ("Zentrix".into(), Some("zentrixx".into()))
            ]
        );
        assert_eq!(
            rows(&fixes("zen tricks", Source::ReadBack, 2), &[]),
            [
                ("Zentrix".into(), None),
                ("Zentrix".into(), Some("zen tricks".into()))
            ]
        );
        assert_eq!(
            rows(&fixes("zentrixx", Source::Manual, 1), &[]),
            [
                ("Zentrix".into(), None),
                ("Zentrix".into(), Some("zentrixx".into()))
            ]
        );
    }
    #[test]
    fn everyday_words_and_letters_never_become_replacements() {
        for heard in ["river", "z"] {
            assert_eq!(
                rows(&fixes(heard, Source::ReadBack, 8), &[]),
                [("Zentrix".into(), None)]
            );
        }
    }
    #[test]
    fn repeated_edits_of_one_dictation_count_once() {
        let mut events = fixes("zentrixx", Source::ReadBack, 1);
        events.push(events[1].clone());
        assert_eq!(rows(&events, &[]), [("Zentrix".into(), None)]);
    }
    #[test]
    fn neighbours_are_split_out_of_terms() {
        let words = Words::from_list(&["we", "use", "triage"]);
        assert_eq!(
            learned_swaps("we use zen tricks treeage", "we use Zentrix triage", &words),
            [Swap {
                heard: "zen tricks".into(),
                wrote: "Zentrix".into(),
                opens_sentence: false
            }]
        );
        assert!(learned_swaps("zentrixx", "Zentrix triage", &words).is_empty());
    }
    #[test]
    fn handwritten_replacements_are_not_duplicated() {
        let known = [crate::vocabulary::Entry {
            term: "ZENTRIX".into(),
            heard: Some("ZENTRIXX".into()),
            weight: 1.0,
        }];
        assert!(rows(&fixes("zentrixx", Source::Manual, 1), &known).is_empty());
    }
    #[test]
    fn forgetting_is_case_insensitive_and_pair_specific() {
        let mut events = fixes("zentrixx", Source::Manual, 1);
        events.push(Event::Forgotten {
            at: String::new(),
            term: "ZENTRIX".into(),
            heard: Some("ZENTRIXX".into()),
        });
        assert_eq!(rows(&events, &[]), [("Zentrix".into(), None)]);
        events.push(
            serde_json::from_str(r#"{"event":"forgotten","at":"","term":"zentrix"}"#).unwrap(),
        );
        assert!(rows(&events, &[]).is_empty());
        events.push(Event::Learned {
            at: String::new(),
            term: "Zentrix".into(),
            heard: Some("zentrixx".into()),
            from: vec![],
        });
        assert!(!forgotten(&events, "ZENTRIX", Some("zentrixx")));
        assert!(forgotten(&events, "Zentrix", None));
        assert!(forgotten(&events, "Zentrix", Some("zen tricks")));
    }
    #[test]
    fn swaps_find_terms_without_rewrites() {
        assert_eq!(
            swaps(
                "Ask Zentrix to open a pull request on quor vo.",
                "Ask Zentrix to open a pull request on Quorvo."
            ),
            vec![Swap {
                heard: "quor vo".into(),
                wrote: "Quorvo".into(),
                opens_sentence: false,
            }]
        );
        let found = swaps(
            "we moved from zentrixx to quor vo",
            "we moved from Zentrix to Quorvo",
        );
        assert_eq!(
            found
                .iter()
                .map(|s| (s.heard.as_str(), s.wrote.as_str()))
                .collect::<Vec<_>>(),
            [("zentrixx", "Zentrix"), ("quor vo", "Quorvo")]
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
    fn a_name_fixed_together_with_plain_words_is_learned_alone() {
        let words = Words::from_list(&[
            "is", "there", "a", "t", "three", "tree", "air", "triage", "skill", "whisper", "flow",
            "the",
        ]);
        let learned = learned_swaps(
            "is there a t three tree air skill",
            "is there a T3 triage skill",
            &words,
        );
        assert_eq!(
            learned
                .iter()
                .map(|s| (s.heard.as_str(), s.wrote.as_str()))
                .collect::<Vec<_>>(),
            [("t three", "T3")],
            "a lone letter is a spelled-out name, so it needs no neighbour"
        );
        let whole = learned_swaps(
            "use the vel tricks flo app",
            "use the Veltrix Flow app",
            &words,
        );
        assert_eq!(
            whole
                .iter()
                .map(|s| (s.heard.as_str(), s.wrote.as_str()))
                .collect::<Vec<_>>(),
            [("vel tricks flo", "Veltrix Flow")],
            "a name of several words stays whole"
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
    fn clipped_branch_names_and_paths_are_not_terms() {
        let words = Words::from_list(&[
            "status", "page", "surface", "website", "links", "project", "drawers", "web", "apps",
            "sandbox", "walls", "through", "day",
        ]);
        // One element per line, as the window walk reads T3 Code's sidebar:
        // a long branch name is two elements, the second opening mid-word.
        let screen = "PR\nzentrix.example\nt3code/project-lat\nest-status\n\
                      t3code/mcp-eval-la\nnding-page\nt3code/simplify-produ\nct-surface\n\
                      t3code/quorvo-agen\ncy-website\nhort-links\napps/web/sandbox/drawers\n\
                      tmp/codex-bins-dir\nUsers/invented/.t3/worktrees/william\n\
                      A/B\nUSB-C\ngpt-6-sol\n7-day\nwalls-through-Zentrix\nZentrix\n";
        assert_eq!(
            context_terms(screen, &words),
            [
                "PR",
                "zentrix.example",
                "A/B",
                "USB-C",
                "gpt-6-sol",
                "7-day",
                "walls-through-Zentrix",
                "Zentrix"
            ]
        );
    }
}
