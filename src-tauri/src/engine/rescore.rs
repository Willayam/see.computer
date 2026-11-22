use crate::boost::Pieces;
use parakeet_rs::SpanCandidate;

pub(super) struct Name {
    spelling: String,
    spoken: Vec<char>,
    tokens: Vec<usize>,
}

impl Name {
    pub(super) fn new(text: &str, pieces: &Pieces) -> Option<Self> {
        Some(Self {
            spelling: text.to_owned(),
            spoken: crate::learn::spoken(text),
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
    let mut out = Vec::new();
    for start in 0..words.len() {
        let mut heard = String::new();
        for end in start..(start + 3).min(words.len()) {
            if end > start {
                if words[end - 1].1 != words[end].0 {
                    break;
                }
                heard.push(' ');
            }
            heard.push_str(&words[end].2);
            let spoken = crate::learn::spoken(&heard);
            if spoken.len() < 3 {
                continue;
            }
            for name in names {
                if heard
                    .split_whitespace()
                    .any(|w| w.eq_ignore_ascii_case(&name.spelling))
                    || (spoken.len() > name.spoken.len() && spoken.starts_with(&name.spoken))
                    || heard.eq_ignore_ascii_case(&name.spelling)
                    || !close(&spoken, &name.spoken)
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

fn close(a: &[char], b: &[char]) -> bool {
    let n = a.len().max(b.len());
    if a.len().abs_diff(b.len()) * 3 > n {
        return false;
    }
    crate::learn::sound_cost(a, b) <= n as f32 * 0.27
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_pieces_and_words_but_stops_at_punctuation() {
        let names = [Name {
            spelling: "Norvatek".into(),
            spoken: crate::learn::spoken("Norvatek"),
            tokens: vec![3],
        }];
        let found = candidates(&["▁Nor", "va", "▁tek"], &names);
        assert!(found.iter().any(|c| c.span == (0..3) && c.tokens == [3]));
        let split = candidates(&["▁Nor", "va", ".", "▁tek"], &names);
        assert!(split.iter().all(|c| c.span.end <= 2 || c.span.start >= 3));
        assert!(candidates(&["▁Norvatek"], &names).is_empty());
    }

    #[test]
    fn preserves_longer_words_and_spans_that_already_contain_the_name() {
        let names = [Name {
            spelling: "Runa".into(),
            spoken: crate::learn::spoken("Runa"),
            tokens: vec![1],
        }];
        assert!(candidates(&["▁runas"], &names).is_empty());
        assert!(candidates(&["▁a", "▁Runa"], &names).is_empty());
    }
}
