use crate::boost::Pieces;
use parakeet_rs::SpanCandidate;

pub(super) struct Name {
    spelling: String,
    spoken: Vec<char>,
    classes: Vec<u16>,
    tokens: Vec<usize>,
}

impl Name {
    pub(super) fn new(text: &str, pieces: &Pieces) -> Option<Self> {
        let spoken = crate::learn::spoken(text);
        let classes = spoken
            .iter()
            .map(|&c| crate::learn::sound_class(c))
            .collect();
        Some(Self {
            spelling: text.to_owned(),
            spoken,
            classes,
            tokens: pieces.encode(text)?,
        })
    }
}

pub(super) fn candidates(pieces: &[&str], names: &[Name]) -> Vec<SpanCandidate> {
    let mut words: Vec<(usize, usize, String)> = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let text = piece.trim_start_matches('▁');
        if !text.chars().all(char::is_alphanumeric) {
            continue;
        }
        if let Some(word) = words.last_mut() {
            if !piece.starts_with('▁') && word.1 == index {
                word.2.push_str(text);
                word.1 += 1;
                continue;
            }
        }
        words.push((index, index + 1, text.to_owned()));
    }
    let word_spoken: Vec<(Vec<char>, Vec<u16>)> = words
        .iter()
        .map(|(_, _, text)| {
            let spoken = crate::learn::spoken(text);
            let classes = spoken
                .iter()
                .map(|&c| crate::learn::sound_class(c))
                .collect();
            (spoken, classes)
        })
        .collect();
    let mut out = Vec::new();
    let mut heard = String::new();
    let mut spoken = Vec::new();
    let mut classes = Vec::new();
    for start in 0..words.len() {
        heard.clear();
        spoken.clear();
        classes.clear();
        for end in start..(start + 3).min(words.len()) {
            if end > start {
                if words[end - 1].1 != words[end].0 {
                    break;
                }
                heard.push(' ');
            }
            heard.push_str(&words[end].2);
            spoken.extend_from_slice(&word_spoken[end].0);
            classes.extend_from_slice(&word_spoken[end].1);
            if spoken.len() < 3 {
                continue;
            }
            for name in names {
                if !close(&spoken, &classes, &name.spoken, &name.classes)
                    || (spoken.len() > name.spoken.len() && spoken.starts_with(&name.spoken))
                    || heard.eq_ignore_ascii_case(&name.spelling)
                    || heard
                        .split_whitespace()
                        .any(|w| w.eq_ignore_ascii_case(&name.spelling))
                {
                    continue;
                }
                out.push(SpanCandidate {
                    span: words[start].0..words[end].1,
                    tokens: name.tokens.clone(),
                });
            }
        }
    }
    out
}

fn close(a: &[char], a_cls: &[u16], b: &[char], b_cls: &[u16]) -> bool {
    let n = a.len().max(b.len());
    if a.len().abs_diff(b.len()) * 3 > n {
        return false;
    }
    crate::learn::sound_cost_within(a, a_cls, b, b_cls, n as f32 * 0.27)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_name(spelling: &str, token: usize) -> Name {
        let spoken = crate::learn::spoken(spelling);
        let classes = spoken
            .iter()
            .map(|&c| crate::learn::sound_class(c))
            .collect();
        Name {
            spelling: spelling.into(),
            spoken,
            classes,
            tokens: vec![token],
        }
    }

    #[test]
    fn joins_pieces_and_words_but_stops_at_punctuation() {
        let names = [test_name("Norvatek", 3)];
        let found = candidates(&["▁Nor", "va", "▁tek"], &names);
        assert!(found.iter().any(|c| c.span == (0..3) && c.tokens == [3]));
        let split = candidates(&["▁Nor", "va", ".", "▁tek"], &names);
        assert!(split.iter().all(|c| c.span.end <= 2 || c.span.start >= 3));
        assert!(candidates(&["▁Norvatek"], &names).is_empty());
    }

    #[test]
    fn preserves_longer_words_and_spans_that_already_contain_the_name() {
        let names = [test_name("Runa", 1)];
        assert!(candidates(&["▁runas"], &names).is_empty());
        assert!(candidates(&["▁a", "▁Runa"], &names).is_empty());
    }

    #[test]
    fn banded_sound_cost_matches_exact_distance() {
        let alphabet: Vec<char> = "abcdefghijklmnopqrstuvwxyzåäö".chars().collect();
        let mut seed = 0x1234_5678_9abc_def0_u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 33) as usize
        };
        for _ in 0..2000 {
            let la = 1 + next() % 14;
            let lb = 1 + next() % 14;
            let a: Vec<char> = (0..la).map(|_| alphabet[next() % alphabet.len()]).collect();
            let b: Vec<char> = (0..lb).map(|_| alphabet[next() % alphabet.len()]).collect();
            let ac: Vec<u16> = a.iter().map(|&c| crate::learn::sound_class(c)).collect();
            let bc: Vec<u16> = b.iter().map(|&c| crate::learn::sound_class(c)).collect();
            let n = la.max(lb);
            let limit = n as f32 * 0.27;
            let exact = crate::learn::sound_cost(&a, &b) <= limit;
            let fast = crate::learn::sound_cost_within(&a, &ac, &b, &bc, limit);
            assert_eq!(fast, exact, "mismatch on {a:?} vs {b:?}");
        }
    }
}
