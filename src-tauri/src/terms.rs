use crate::align::strip;
use crate::learn::Words;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub text: String,
    pub source: &'static str,
}

/// At most this many terms from the focused window, the most frequent first.
const SCREEN_TERMS: usize = 32;
/// Daybook describes the last two hours, so it gets fewer and ranks last.
const DAYBOOK_TERMS: usize = 8;
/// Every other unusual word or pair on screen, for matching after decoding
/// only. Boosting this many terms would pull ordinary speech toward them.
const CANDIDATES: usize = 400;

/// The terms one dictation gets from what is on screen and from Daybook.
/// Live capture and the eval both come through here.
pub fn from_screen(text: &str, daybook: &[String]) -> Vec<Term> {
    let words = Words::shared();
    warm_screen_words(text, words);
    gather(
        &context_terms(text, words),
        &screen_candidates(text, words),
        daybook,
    )
}

/// Screen terms, then Daybook ones, each ranked and capped, then the
/// candidates that only post-decode matching reads. No spelling twice.
pub fn gather(screen: &[String], candidates: &[String], daybook: &[String]) -> Vec<Term> {
    let mut seen = HashSet::new();
    screen
        .iter()
        .take(SCREEN_TERMS)
        .map(|text| (text, "screen"))
        .chain(
            daybook
                .iter()
                .take(DAYBOOK_TERMS)
                .map(|text| (text, "daybook")),
        )
        .chain(candidates.iter().map(|text| (text, "candidate")))
        .filter(|(text, _)| seen.insert(text.to_lowercase()))
        .take(SCREEN_TERMS + DAYBOOK_TERMS + CANDIDATES)
        .map(|(text, source)| Term {
            text: text.clone(),
            source,
        })
        .collect()
}

impl Term {
    /// Whether the decoder is pushed toward this term while it decodes.
    pub fn boosted(&self) -> bool {
        self.source != "candidate"
    }
}

pub fn near_misses(text: &str, terms: &[Term], saved: &[&str], words: &Words) -> String {
    crate::sounds_like::rewrite(text, terms, saved, words)
}

/// Pre-warm `words` with every token `context_terms` and `screen_candidates`
/// will query, so one batched spellcheck pass populates the shared cache.
pub fn warm_screen_words(text: &str, words: &Words) {
    let mut unique = HashSet::new();
    let mut batch = Vec::new();
    let mut push = |word: &str| {
        if !word.is_empty() && unique.insert(word.to_owned()) {
            batch.push(word.to_owned());
        }
    };
    for line in text.lines() {
        let mut prev_cap_plain = false;
        for part in line.split_whitespace() {
            let whole = strip(part);
            let token = strip(whole.split(['\'', '’']).next().unwrap_or_default());
            let contraction_lower =
                token.len() < whole.len() && token.chars().all(char::is_lowercase);
            let is_cap_plain = !contraction_lower
                && token.len() <= 64
                && token.chars().count() >= 3
                && token.chars().all(char::is_alphabetic)
                && token.chars().next().is_some_and(char::is_uppercase);
            if contraction_lower || token.len() > 64 {
                prev_cap_plain = false;
                continue;
            }
            let letters = token.chars().filter(|c| c.is_alphabetic()).count();
            if letters < 2 {
                prev_cap_plain = false;
                continue;
            }
            if token.chars().all(char::is_alphabetic) {
                let lower = token.to_lowercase();
                if !ORDINARY.contains(&lower.as_str()) {
                    let upper = token.chars().filter(|c| c.is_uppercase()).count();
                    let lower_n = token.chars().filter(|c| c.is_lowercase()).count();
                    let capital_inside = token.chars().skip(1).any(char::is_uppercase);
                    if upper > 0 && !(capital_inside && lower_n > 0) {
                        push(token);
                    }
                    if letters >= 3 || prev_cap_plain {
                        push(&lower);
                    }
                }
            } else if token.contains('-')
                && !token.contains(['/', '.', '_'])
                && !token.chars().any(char::is_numeric)
            {
                for piece in token.split('-') {
                    if !piece.is_empty() {
                        push(piece);
                    }
                }
            }
            prev_cap_plain = is_cap_plain;
        }
    }
    words.warm_quickly(&batch);
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

/// Every unusual word on screen and every pair a capitalized word opens, for
/// matching a misheard span after decoding. Wider than `context_terms`: it
/// keeps lowercase product words ("kanban") and two-word names ("Quillo Bot"),
/// and does not care how often a word shows.
pub fn screen_candidates(text: &str, words: &Words) -> Vec<String> {
    let mut known: HashMap<String, bool> = HashMap::new();
    let mut ordinary = |token: &str| {
        let lower = token.to_lowercase();
        *known
            .entry(lower.clone())
            .or_insert_with(|| ORDINARY.contains(&lower.as_str()) || words.knows_quickly(&lower))
    };
    // "The" and "New" open UI labels everywhere, but prose also shows them
    // lowercase. A name never does.
    let lowercase: HashSet<&str> = text
        .split_whitespace()
        .map(strip)
        .filter(|token| token.chars().next().is_some_and(char::is_lowercase))
        .collect();
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut keep = |candidate: String, found: &mut Vec<String>| {
        if seen.insert(candidate.to_lowercase()) {
            found.push(candidate);
        }
    };
    for line in text.lines() {
        // "didn't" is a word, but its stem "didn" sounds like "don".
        let tokens: Vec<(&str, bool, bool)> = line
            .split_whitespace()
            .map(|part| {
                let whole = strip(part);
                let stem = strip(whole.split(['\'', '’']).next().unwrap_or_default());
                let token = if stem.len() < whole.len() && stem.chars().all(char::is_lowercase) {
                    ""
                } else {
                    stem
                };
                (
                    token,
                    part.ends_with(['.', '!', '?', ':']),
                    part.ends_with(char::is_alphanumeric),
                )
            })
            .collect();
        let mut opens = true;
        for (index, &(token, closes, bare)) in tokens.iter().enumerate() {
            let opened = opens;
            opens = closes;
            let letters = token.chars().filter(|c| c.is_alphabetic()).count();
            if letters < 3 || token.len() > 64 {
                continue;
            }
            let plain = token.chars().all(char::is_alphabetic);
            // A capital mid-sentence marks a name even when the word list
            // knows the word: "Kindle", "Slack".
            let capital_mid_sentence = !opened
                && plain
                && letters >= 4
                && token.chars().next().is_some_and(char::is_uppercase)
                && token.chars().skip(1).all(char::is_lowercase)
                && !lowercase.contains(token.to_lowercase().as_str());
            let unusual = capital_mid_sentence
                || names_something(token, false, words)
                || (plain && !ordinary(token));
            if unusual {
                keep(token.to_owned(), &mut found);
            }
            let Some(&(next, _, _)) = tokens.get(index + 1) else {
                continue;
            };
            let capitalized = |word: &str| word.chars().next().is_some_and(char::is_uppercase);
            // "Pellmark. Click" and "Pellmark, Click" are two things.
            if capitalized(token)
                && plain
                && bare
                && next.chars().count() >= 2
                && next.chars().all(char::is_alphabetic)
                && (capitalized(next) || !ordinary(next))
                && (unusual || !ordinary(next))
            {
                keep(format!("{token} {next}"), &mut found);
            }
        }
    }
    found
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_keeps_names_and_drops_labels_and_prose() {
        let words = Words::from_list(&[
            "and",
            "pro",
            "comparing",
            "settings",
            "the",
            "file",
            "to",
            "sent",
            "i",
            "follow",
            "up",
            "right",
            "new",
            "on",
            "prices",
            "a",
            "with",
            "yesterday",
            "convex",
            "we",
            "store",
            "rows",
            "now",
            "chat",
            "agent",
            "yeah",
            "it",
            "came",
            "out",
            "as",
            "from",
            "quiet",
            "audio",
        ]);
        let screen = "Settings\nKlarna\nI sent the file to Larsten yesterday. And then Larsten replied.\n\
                      Comparing prices on Prisjakt.nu with a MacBook Pro, a dropdown and a follow-up.\n\
                      RIGHT 3D T3 n8n API, Sep 3 PM. I'll ask Larsten's team.\n\
                      We store the rows in Convex now.\nNew Chat With The Agent\n\
                      It came out as \u{201c}Yeah.\u{201d} and \u{201c}Okay.\u{201d} from quiet audio.";
        assert_eq!(
            context_terms(screen, &words),
            ["Larsten", "Klarna", "Prisjakt.nu", "MacBook", "n8n", "API"]
        );
        let daybook = ["Larsten".to_owned(), "Klarna".to_owned()];
        let screen: Vec<String> = (0..40).map(|n| format!("Term{n}")).collect();
        let terms = gather(&screen, &[], &daybook);
        assert_eq!(terms.len(), SCREEN_TERMS + 2);
        assert!(terms[..SCREEN_TERMS].iter().all(|t| t.source == "screen"));
        assert_eq!(terms[SCREEN_TERMS].text, "Larsten");
        let terms = gather(&["Larsten".to_owned()], &[], &daybook);
        assert_eq!(
            terms,
            [
                Term {
                    text: "Larsten".into(),
                    source: "screen"
                },
                Term {
                    text: "Klarna".into(),
                    source: "daybook"
                },
            ]
        );
    }

    #[test]
    fn candidates_keep_lowercase_products_pairs_and_capitals_mid_sentence() {
        let words = Words::from_list(&[
            "open", "the", "board", "in", "and", "ask", "about", "it", "bot", "finance", "kindle",
            "settings", "we", "use", "click", "new", "didn't",
        ]);
        let screen = "Settings\nOpen the zorboard in Quillo Bot and ask Kindle about it.\n\
                      We use Pellmark Finance. Click New Board, we didn't open it.\nnew board";
        let candidates = screen_candidates(screen, &words);
        for expected in [
            "zorboard",
            "Quillo",
            "Quillo Bot",
            "Kindle",
            "Pellmark Finance",
        ] {
            assert!(
                candidates.contains(&expected.to_owned()),
                "{expected} in {candidates:?}"
            );
        }
        for ordinary in [
            "Settings",
            "Open",
            "We",
            "board",
            "Bot and",
            "Kindle about",
            "New",
            "didn",
            "Finance Click",
        ] {
            assert!(
                !candidates.contains(&ordinary.to_owned()),
                "{ordinary} in {candidates:?}"
            );
        }
        let terms = gather(&["Quillo".to_owned()], &candidates, &[]);
        assert_eq!(terms.iter().filter(|term| term.text == "Quillo").count(), 1);
        assert!(terms[0].boosted());
        assert!(terms[1..].iter().all(|term| !term.boosted()));
    }

    #[test]
    fn a_near_miss_of_an_on_screen_name_is_written_as_the_name() {
        let words = Words::from_list(&["i", "sent", "the", "file", "to", "larson", "frida"]);
        let screen = |names: &[&str]| -> Vec<Term> {
            names
                .iter()
                .map(|text| Term {
                    text: (*text).into(),
                    source: "screen",
                })
                .collect()
        };
        let fix = |text: &str, terms: &[Term]| near_misses(text, terms, &[], &words);
        let terms = screen(&["Larsten", "Frida"]);
        assert_eq!(
            fix("I sent the file to Larston yesterday.", &terms),
            "I sent the file to Larsten yesterday."
        );
        assert_eq!(fix("Ask Lasrten.", &terms), "Ask Larsten.");
        // A known word, a letter added or dropped, and lowercase stay as heard.
        assert_eq!(
            fix("Larson and Fridag and larston", &terms),
            "Larson and Fridag and larston"
        );
        // Two names it could be is no answer, and Daybook is not the screen.
        assert_eq!(fix("Larston", &screen(&["Larsten", "Larstan"])), "Larston");
        // A saved vocabulary word is what the user meant.
        let kvittu = screen(&["Kvittu"]);
        assert_eq!(fix("Kvitto", &kvittu), "Kvittu");
        assert_eq!(
            near_misses("Kvitto", &kvittu, &["kvitto"], &words),
            "Kvitto"
        );
        let daybook = [Term {
            text: "Larsten".into(),
            source: "daybook",
        }];
        assert_eq!(fix("Larston", &daybook), "Larston");
    }

    #[test]
    fn screen_term_can_land_only_in_the_boosted_hunk() {
        let words = Words::from_list(&["use", "the", "project"]);
        let extracted = context_terms("use the Zorvia project", &words);
        assert_eq!(extracted, ["Zorvia"]);
        let terms = extracted
            .into_iter()
            .map(|text| crate::boost::Term { text, weight: 0.5 })
            .collect::<Vec<_>>();
        assert_eq!(
            crate::boost::merge("Use the Zorvia project", "Use the Zorvia project", &terms),
            "Use the Zorvia project"
        );
        assert_eq!(
            crate::boost::merge("Use the Sylvia project", "Use the Zorvia project", &terms),
            "Use the Zorvia project"
        );
        assert_eq!(
            crate::boost::merge("Use the Sylvia project", "Use the Zorvia project", &[]),
            "Use the Sylvia project"
        );
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

    #[test]
    fn field_text_boosts_names_without_boosting_ordinary_words() {
        let words = Words::from_list(&["hey", "the", "invoice", "is", "late"]);
        let nearby = "Hey Kavilo, the Pellmark invoice is late\n";
        let terms = gather(
            &context_terms(nearby, &words),
            &screen_candidates(nearby, &words),
            &[],
        );
        let boosted: Vec<_> = terms
            .iter()
            .filter(|term| term.boosted())
            .map(|term| term.text.as_str())
            .collect();
        assert_eq!(boosted, ["Kavilo", "Pellmark"]);
    }
}
