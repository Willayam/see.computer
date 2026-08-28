//! Dropping the sounds you make while thinking.
//!
//! Parakeet transcribes hesitation faithfully, so 3% of the words it hands back
//! are `um` and `uh`. Nobody wants those pasted. This is a deterministic pass
//! over whole words, not a rewrite: it deletes a fixed set of hesitation sounds
//! and repairs the spacing and the capital letter they were holding.
//!
//! What it deliberately leaves alone: hedges like `like`, `actually` and
//! `basically`, which are real words far more often than they are filler, and
//! `er`, which is a common Swedish word. Repeated-word stutters are not handled
//! either; there were eight in the last five thousand dictated words, and half
//! of those were emphasis.

/// Removes hesitation sounds, then closes the gap they leave.
///
/// Returns an empty string when the whole utterance was a hum, which the caller
/// reads as nothing heard.
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut capitalise_next = false;
    for (space, word) in words(text) {
        if is_filler(word) {
            // The filler was carrying the sentence's capital letter. Whichever
            // word takes its place has to carry it instead.
            capitalise_next |= opens_a_sentence(&out) && starts_uppercase(word);
            continue;
        }
        out.push_str(space);
        if std::mem::take(&mut capitalise_next) {
            push_capitalised(&mut out, word);
        } else {
            out.push_str(word);
        }
    }
    out.trim().to_owned()
}

/// Splits into words, each carrying the whitespace that came before it, so the
/// original spacing survives everywhere the pass does not touch.
fn words(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let split = rest.find(|c: char| !c.is_whitespace())?;
        let (space, tail) = rest.split_at(split);
        let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let (word, remainder) = tail.split_at(end);
        rest = remainder;
        Some((space, word))
    })
}

/// `Hmm.` and `um,` arrive with their punctuation attached, and it goes with
/// them. Punctuation inside the word stays, which is what keeps the affirmative
/// `Mm-hmm` from reading as a hum.
fn is_filler(word: &str) -> bool {
    let core = word.trim_matches(|c: char| !c.is_alphanumeric());
    let mut chars = core.chars().flat_map(char::to_lowercase);
    let tail = match (chars.next(), chars.next()) {
        (Some('u'), Some('m')) | (Some('h'), Some('m')) => 'm',
        (Some('u'), Some('h')) => 'h',
        _ => return false,
    };
    // Only the last letter may run on, so `umm` goes and `uhoh` stays.
    chars.all(|c| c == tail)
}

fn opens_a_sentence(out: &str) -> bool {
    match out.trim_end().chars().next_back() {
        None => true,
        Some(c) => matches!(c, '.' | '!' | '?'),
    }
}

fn starts_uppercase(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_uppercase)
}

fn push_capitalised(out: &mut String, word: &str) {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
        None => out.push_str(word),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_hesitation_and_closes_the_gap() {
        assert_eq!(
            strip("It should um lead into the section"),
            "It should lead into the section"
        );
    }

    #[test]
    fn drops_a_run_of_them() {
        assert_eq!(
            strip("kind of um uh making it smaller"),
            "kind of making it smaller"
        );
    }

    #[test]
    fn takes_the_comma_with_it() {
        assert_eq!(strip("Well, um, that's uh fine"), "Well, that's fine");
    }

    #[test]
    fn hands_the_capital_to_the_next_word() {
        assert_eq!(
            strip("Um and I think you should try"),
            "And I think you should try"
        );
        assert_eq!(
            strip("One is interesting. Um and I think so too"),
            "One is interesting. And I think so too"
        );
    }

    #[test]
    fn leaves_a_sentence_that_already_starts_capitalised() {
        assert_eq!(
            strip("Um I think for me to release this"),
            "I think for me to release this"
        );
    }

    #[test]
    fn a_whole_utterance_of_humming_comes_back_empty() {
        assert_eq!(strip("Hmm."), "");
        assert_eq!(strip("Hm. Um, uh."), "");
    }

    #[test]
    fn spares_words_that_merely_contain_one() {
        for word in ["thumb", "uhoh", "human", "hummus", "umbrella"] {
            assert_eq!(strip(word), word);
        }
    }

    #[test]
    fn spares_the_affirmative_mm_hmm() {
        assert_eq!(strip("Mm-hmm."), "Mm-hmm.");
    }

    #[test]
    fn spares_er_because_swedish_needs_it() {
        assert_eq!(strip("Det är er tur"), "Det är er tur");
    }

    #[test]
    fn spares_the_hedges_it_is_not_here_to_judge() {
        let said = "It's like actually basically fine";
        assert_eq!(strip(said), said);
    }

    #[test]
    fn keeps_the_spacing_it_did_not_touch() {
        assert_eq!(
            strip("first line\nsecond um line"),
            "first line\nsecond line"
        );
    }

    #[test]
    fn leaves_clean_speech_untouched() {
        let said = "Push the words you keep saying into the decoder.";
        assert_eq!(strip(said), said);
    }
}
