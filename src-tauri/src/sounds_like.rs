fn near_miss<'a>(word: &str, names: &[&'a str], words: &crate::learn::Words) -> Option<&'a str> {
    let mut chars = word.chars();
    let capitalized = chars.next().is_some_and(char::is_uppercase) && chars.all(char::is_lowercase);
    if !capitalized || word.chars().count() < 5 || words.knows_quickly(word) {
        return None;
    }
    let lower = word.to_lowercase();
    let prefix: String = lower.chars().take(2).collect();
    let mut found = names.iter().filter(|name| {
        let name = name.to_lowercase();
        name != lower && name.starts_with(&prefix) && one_letter(&lower, &name)
    });
    match (found.next(), found.next()) {
        (Some(name), None) => Some(name),
        _ => None,
    }
}

/// The same length, and one letter changed or two neighbours swapped.
fn one_letter(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() != b.len() {
        return false;
    }
    let differ: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
    match differ.as_slice() {
        [_] => true,
        [i, j] => j - i == 1 && a[*i] == b[*j] && a[*j] == b[*i],
        _ => false,
    }
}

fn key(text: &str) -> String {
    let mut result = String::new();
    let mut last = None;
    for c in text.to_lowercase().chars().filter(|c| c.is_alphanumeric()) {
        let sound = match c {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'ä' | 'å' | 'ö' => 'a',
            'c' | 'k' | 'q' => 'k',
            'w' | 'v' => 'v',
            'z' | 's' => 's',
            'b' | 'p' => 'p',
            't' | 'd' => 't',
            _ => c,
        };
        if last != Some(sound) {
            result.push(sound);
        }
        last = Some(sound);
    }
    result
}

fn spelling(text: &str) -> String {
    text.to_lowercase().replace(' ', "")
}

fn protected_prefix(text: &str, name: &str) -> Option<usize> {
    let prefix = text.get(..name.len())?;
    (!prefix.is_empty()
        && prefix.eq_ignore_ascii_case(name)
        && !text[name.len()..].starts_with(char::is_alphanumeric))
    .then_some(name.len())
}

pub fn rewrite(
    text: &str,
    terms: &[crate::context::Term],
    saved: &[&str],
    words: &crate::learn::Words,
) -> String {
    let names: Vec<_> = terms
        .iter()
        .filter(|t| {
            t.source != "daybook" && t.text.chars().all(|c| c.is_alphanumeric() || c == ' ')
        })
        // OCR leaves lowercase fragments ("buit", "ound") among the candidates.
        .filter(|t| t.source != "candidate" || t.text.starts_with(char::is_uppercase))
        .map(|t| t.text.as_str())
        .collect();
    if names.is_empty() {
        return text.to_owned();
    }
    let candidates: Vec<_> = names
        .iter()
        .map(|&name| (name, key(name), spelling(name)))
        .collect();
    let protected: Vec<_> = saved.iter().chain(names.iter()).copied().collect();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(char::is_alphanumeric) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric())
            .unwrap_or(rest.len());
        let word = &rest[..end];
        if let Some(length) = protected
            .iter()
            .filter_map(|name| protected_prefix(rest, name))
            .max()
        {
            out.push_str(&rest[..length]);
            rest = &rest[length..];
            continue;
        }
        let mut ends = vec![end];
        if word.chars().count() >= 2 && word.starts_with(char::is_uppercase) {
            for _ in 1..3 {
                let after = &rest[*ends.last().unwrap()..];
                let Some(next) = after.strip_prefix(' ') else {
                    break;
                };
                let len = next
                    .find(|c: char| !c.is_alphanumeric())
                    .unwrap_or(next.len());
                if len == 0
                    || protected
                        .iter()
                        .any(|name| protected_prefix(next, name).is_some())
                {
                    break;
                }
                ends.push(rest.len() - next.len() + len);
            }
        }
        let mut used = end;
        let mut replacement = None;
        if word.starts_with(char::is_uppercase) {
            for n in ends.into_iter().rev() {
                let span = &rest[..n];
                // A short word alone ("Can", "And") sounds like too much.
                if n == end && word.chars().count() < 4 {
                    continue;
                }
                let sound = key(span);
                if sound.chars().count() < 3 {
                    continue;
                }
                let heard = spelling(span);
                let mut hits = candidates.iter().filter(|(_, k, letters)| {
                    *k == sound && strsim::osa_distance(letters, &heard) <= 2
                });
                if let (Some((name, _, _)), None) = (hits.next(), hits.next()) {
                    replacement = Some(*name);
                    used = n;
                    break;
                }
            }
        }
        if replacement.is_none() {
            replacement = near_miss(word, &names, words);
        }
        out.push_str(replacement.unwrap_or(word));
        rest = &rest[used..];
    }
    out.push_str(rest);
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn joins_a_split_name_but_respects_punctuation() {
        let term = crate::context::Term {
            text: "Kavilo".into(),
            source: "screen",
        };
        let words = crate::learn::Words::from_list(&[]);
        let fix = |s| rewrite(s, std::slice::from_ref(&term), &[], &words);
        assert_eq!(fix("Cav illo!"), "Kavilo!");
        assert_eq!(fix("Cav, illo!"), "Cav, illo!");
    }

    #[test]
    fn candidates_must_look_like_names_and_short_words_stay() {
        let term = |text: &str| crate::context::Term {
            text: text.into(),
            source: "candidate",
        };
        let words = crate::learn::Words::from_list(&[]);
        let fragment = [term("buit")];
        assert_eq!(rewrite("But it", &fragment, &[], &words), "But it");
        let name = [term("Kann")];
        assert_eq!(rewrite("Can we", &name, &[], &words), "Can we");
        let name = [term("Kavilo")];
        assert_eq!(rewrite("Cavillo said", &name, &[], &words), "Kavilo said");
    }

    #[test]
    fn keeps_ambiguous_names_and_saved_phrases() {
        let terms = ["Kavilo", "Cavillo"].map(|text| crate::context::Term {
            text: text.into(),
            source: "screen",
        });
        let words = crate::learn::Words::from_list(&[]);
        assert_eq!(rewrite("Cav ilo", &terms, &[], &words), "Cav ilo");
        assert_eq!(
            rewrite("Cav ilo", &terms[..1], &["Cav ilo"], &words),
            "Cav ilo"
        );
    }
}
