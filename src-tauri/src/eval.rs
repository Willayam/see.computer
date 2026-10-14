use std::path::{Path, PathBuf};

pub struct Clip {
    pub line: usize,
    pub written_path: String,
    pub path: PathBuf,
    pub reference: String,
}

pub fn parse_manifest(path: &Path, text: &str) -> (Vec<Clip>, Vec<String>) {
    let mut clips = Vec::new();
    let mut errors = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((written_path, reference)) = line.split_once('\t') else {
            errors.push(format!("line {}: expected a tab", index + 1));
            continue;
        };
        let wav = Path::new(written_path);
        clips.push(Clip {
            line: index + 1,
            written_path: written_path.to_owned(),
            path: if wav.is_absolute() {
                wav.to_path_buf()
            } else {
                path.parent().unwrap_or(Path::new("")).join(wav)
            },
            reference: reference.to_owned(),
        });
    }
    (clips, errors)
}

#[derive(Default)]
pub struct Score {
    pub words: usize,
    pub errors: usize,
    pub hits: usize,
    pub occurrences: usize,
    pub insertions: usize,
}

impl Score {
    pub fn wer(&self) -> f64 {
        if self.words == 0 {
            0.0
        } else {
            self.errors as f64 / self.words as f64
        }
    }

    pub fn add(&mut self, other: &Score) {
        self.words += other.words;
        self.errors += other.errors;
        self.hits += other.hits;
        self.occurrences += other.occurrences;
        self.insertions += other.insertions;
    }
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '\'' {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn occurrences(text: &str, term: &str, case_sensitive: bool) -> usize {
    if term.is_empty() {
        return 0;
    }
    let (haystack, needle) = if case_sensitive {
        (text.to_owned(), term.to_owned())
    } else {
        (text.to_lowercase(), term.to_lowercase())
    };
    haystack
        .match_indices(&needle)
        .filter(|(start, matched)| {
            let before = haystack[..*start].chars().next_back();
            let after = haystack[start + matched.len()..].chars().next();
            before.is_none_or(|ch| !ch.is_alphanumeric())
                && after.is_none_or(|ch| !ch.is_alphanumeric())
        })
        .count()
}

pub fn score(reference: &str, hypothesis: &str, terms: &[String]) -> Score {
    let reference_words = words(reference);
    let hypothesis_words = words(hypothesis);
    let mut previous: Vec<usize> = (0..=hypothesis_words.len()).collect();
    for (i, expected) in reference_words.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, actual) in hypothesis_words.iter().enumerate() {
            current.push(
                (previous[j + 1] + 1)
                    .min(current[j] + 1)
                    .min(previous[j] + usize::from(expected != actual)),
            );
        }
        previous = current;
    }
    let mut result = Score {
        words: reference_words.len(),
        errors: previous[hypothesis_words.len()],
        ..Score::default()
    };
    for term in terms {
        let expected = occurrences(reference, term, false);
        let actual = occurrences(hypothesis, term, true);
        result.occurrences += expected;
        result.hits += expected.min(actual);
        result.insertions += actual.saturating_sub(expected);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_errors() {
        assert_eq!(score("same words", "same words", &[]).errors, 0);
        let result = score("the cat sat", "the bat sat on", &[]);
        assert_eq!((result.errors, result.words), (2, 3));
        assert_eq!(score("Hello, world.", "hello world", &[]).errors, 0);
        assert_eq!(score("Å ä ö", "å ä ö", &[]).errors, 0);
        assert_eq!(score("Å ä ö", "a a o", &[]).errors, 3);
    }

    #[test]
    fn terms_and_insertions() {
        let terms = vec!["CLAUDE.md".into(), "Wispr Flow".into(), "pnpm".into()];
        let result = score(
            "CLAUDE.md Wispr Flow pnpm",
            "CLAUDE.md Wispr Flow pnpm",
            &terms,
        );
        assert_eq!(
            (result.hits, result.occurrences, result.insertions),
            (3, 3, 0)
        );
        let result = score("pnpm", "PNPM", &terms);
        assert_eq!((result.hits, result.occurrences), (0, 1));
        let result = score("hello", "pnpm pnpm", &terms);
        assert_eq!(result.insertions, 2);
    }

    #[test]
    fn manifest_lines() {
        let (clips, errors) = parse_manifest(
            Path::new("/tmp/corpus/list.tsv"),
            "# note\n\nclip.wav\tHej\ninvalid\n/elsewhere/a.wav\tHi",
        );
        assert_eq!(errors, ["line 4: expected a tab"]);
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].path, Path::new("/tmp/corpus/clip.wav"));
        assert_eq!(clips[0].written_path, "clip.wav");
        assert_eq!(clips[1].path, Path::new("/elsewhere/a.wav"));
    }
}
