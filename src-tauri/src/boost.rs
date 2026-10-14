//! Custom vocabulary, biased into the decoder rather than patched in after it.
//!
//! Parakeet has never heard `Tauri` or `pnpm`, so it emits the nearest thing it
//! knows and no amount of find-and-replace afterwards can tell `Tauri` from the
//! `tao ry` that a different sentence really did say. The fix is to lean on the
//! decoder while it is still choosing: the terms the user cares about go into a
//! prefix tree over the model's own sentencepiece ids, and every greedy step
//! gets a bonus for tokens that continue a term already underway.
//!
//! This is NeMo's word boosting, shallow-fusion style, and it costs a handful
//! of float adds per frame against an ONNX call we already pay for.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parakeet_rs::TokenBias;

pub(crate) fn occurrences(text: &str, term: &str, case_sensitive: bool) -> usize {
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

pub fn merge(plain: &str, boosted: &str, terms: &[Term]) -> String {
    let a: Vec<_> = plain.split_whitespace().collect();
    let b: Vec<_> = boosted.split_whitespace().collect();
    let gained = |left: &str, right: &str| {
        terms
            .iter()
            .any(|term| occurrences(right, &term.text, true) > occurrences(left, &term.text, true))
    };
    let mut out = Vec::new();
    let (mut ai, mut bi) = (0, 0);
    let mut boosted_hunk = false;
    for (x, y) in crate::learn::alignment(&a, &b)
        .into_iter()
        .chain(std::iter::once((a.len(), b.len())))
    {
        let left = a[ai..x].join(" ");
        let right = b[bi..y].join(" ");
        let gain = gained(&left, &right);
        if !a[ai..x].is_empty() || !b[bi..y].is_empty() {
            boosted_hunk = gain;
        }
        out.extend(if gain { &b[bi..y] } else { &a[ai..x] });
        if x < a.len() {
            out.push(if gained(a[x], b[y]) || (boosted_hunk && a[x] != b[y]) {
                b[y]
            } else {
                a[x]
            });
            if a[x] == b[y] {
                boosted_hunk = false;
            }
        }
        ai = x + 1;
        bi = y + 1;
    }
    out.join(" ")
}

/// How far below the model's own best token a candidate may sit and still be
/// eligible for a bonus, in the units the joint logits happen to use.
///
/// This is the load-bearing one. Without it the bonus goes to every term's
/// opening token whatever the audio said, so the longer the list the likelier
/// one of them wins a step it had no business winning: a sixty-term vocabulary
/// turned `pnpm build` into `PNPM Buildship` and opened a sentence with
/// `Wispr Flow`. With the gate, a term the acoustics rule out is never in the
/// running, and the list's size stops mattering.
const DEFAULT_MARGIN: f32 = 3.0;

/// How hard to push a candidate that is in the running.
///
/// Eight sits in the middle of a plateau: with the margin at three, four
/// through twelve all produce identical output on the jargon clips and leave
/// both fixtures byte-identical, because the gate binds before the scale does.
/// Widening the margin narrows that plateau — at ten it ends by scale six.
const DEFAULT_SCALE: f32 = 8.0;

/// How much more a token deeper inside a term is worth than the one that
/// started it.
///
/// Zero. Growing the bonus with depth is what NeMo's context graph does, and it
/// does help a genuine term finish — but it also entrenches a term the speaker
/// never began, since abandoning one costs more the further in it got. That was
/// how `Buildship` survived. The margin gate now stops most wrong turns from
/// being taken at all, so the extra push has nothing left to buy; it stays a
/// seam rather than a deletion in case a real corpus disagrees.
const DEFAULT_DEPTH: f32 = 0.0;

/// All three are env seams so the eval gate can sweep them without a recompile.
fn tunable(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(default)
}

/// The model's sentencepiece inventory, read from the `vocab.txt` that ships
/// beside the weights.
pub struct Pieces {
    ids: HashMap<String, usize>,
    longest: usize,
}

impl Pieces {
    pub fn load(vocab: &Path) -> Option<Pieces> {
        Pieces::parse(&fs::read_to_string(vocab).ok()?)
    }

    fn parse(text: &str) -> Option<Pieces> {
        let mut ids = HashMap::new();
        let mut longest = 0;
        for line in text.lines() {
            // `vocab.txt` is `<piece> <id>`, and a piece may itself be a space.
            // A line that does not read that way is skipped rather than taken
            // as a reason to give up: one odd entry must not quietly turn all
            // boosting off.
            let Some((piece, id)) = line.rsplit_once(' ') else {
                continue;
            };
            let Ok(id) = id.parse::<usize>() else {
                continue;
            };
            longest = longest.max(piece.chars().count());
            ids.insert(piece.to_owned(), id);
        }
        (!ids.is_empty()).then_some(Pieces { ids, longest })
    }

    /// Sentencepiece ids for `term`, or `None` when some part of it cannot be
    /// spelled out of this vocabulary at all.
    ///
    /// Greedy longest-match rather than the real unigram segmentation. It can
    /// disagree with the model's own tokenization of a term, which costs a
    /// little boosting accuracy and nothing else: a term tokenized slightly
    /// differently simply fails to match and goes unboosted.
    fn encode(&self, term: &str) -> Option<Vec<usize>> {
        let mut out = Vec::new();
        for word in term.split_whitespace() {
            // A word start carries the sentencepiece word-boundary marker.
            let mut rest: Vec<char> = format!("\u{2581}{word}").chars().collect();
            while !rest.is_empty() {
                let mut taken = 0;
                for len in (1..=self.longest.min(rest.len())).rev() {
                    let candidate: String = rest[..len].iter().collect();
                    if let Some(&id) = self.ids.get(&candidate) {
                        out.push(id);
                        taken = len;
                        break;
                    }
                }
                if taken == 0 {
                    return None;
                }
                rest.drain(..taken);
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

/// One boostable term and how hard to push for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub text: String,
    pub weight: f32,
}

/// A phrase the model keeps hearing in place of a term, and the term.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub heard: String,
    pub term: String,
}

/// The terms the user cares about, and where they were read from.
///
/// A plain file in the same folder as the dictation history, in keeping with
/// the rest of the app: something to open and edit, not a settings pane.
pub struct Lexicon {
    path: PathBuf,
    stamps: [Option<SystemTime>; 2],
    terms: Vec<Term>,
    rules: Vec<Rule>,
}

impl Lexicon {
    /// `~/Documents/see.computer/vocabulary.md`, beside the dictation history,
    /// unless `SEE_COMPUTER_VOCABULARY` names a file directly. The learned
    /// words live in `learned.md` beside whichever file this names.
    ///
    /// Falls back to the history's own fallback location when Documents holds
    /// no vocabulary but Application Support does, so a machine that denied
    /// Documents access keeps working the way its history already does.
    pub fn default_path() -> PathBuf {
        if let Some(path) = std::env::var_os("SEE_COMPUTER_VOCABULARY") {
            return PathBuf::from(path);
        }
        const NAME: &str = "vocabulary.md";
        let documents = crate::paths::documents().join(NAME);
        let support = crate::paths::history_fallback().join(NAME);
        if !documents.exists() && support.exists() {
            support
        } else {
            documents
        }
    }

    pub fn load(path: PathBuf) -> Lexicon {
        let mut lexicon = Lexicon {
            path,
            stamps: [None, None],
            terms: Vec::new(),
            rules: Vec::new(),
        };
        lexicon.refresh();
        lexicon
    }

    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    /// Re-read the files when either has changed on disk. Returns whether the
    /// terms or rules moved, so a caller can avoid rebuilding a trie that would
    /// come out the same. A missing file is not an error; it means no custom
    /// vocabulary, or nothing learned, yet.
    pub fn refresh(&mut self) -> bool {
        let paths = [self.path.clone(), self.path.with_file_name("learned.md")];
        let stamps = paths
            .clone()
            .map(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
        if stamps == self.stamps
            && (stamps.iter().any(Option::is_some)
                || (self.terms.is_empty() && self.rules.is_empty()))
        {
            return false;
        }
        self.stamps = stamps;
        let mut terms: Vec<Term> = Vec::new();
        let mut rules = Vec::new();
        for path in paths {
            let (parsed, found) = fs::read_to_string(path)
                .map(|s| parse(&s))
                .unwrap_or_default();
            for term in parsed {
                if let Some(old) = terms.iter_mut().find(|old| old.text == term.text) {
                    old.weight = old.weight.max(term.weight);
                } else {
                    terms.push(term);
                }
            }
            rules.extend(found);
        }
        rules.sort_by_key(|r| std::cmp::Reverse(r.heard.len()));
        let changed = terms != self.terms || rules != self.rules;
        self.terms = terms;
        self.rules = rules;
        changed
    }

    /// Applies every `heard -> Term` rule, longest first, to whole words.
    /// Case-insensitive for ASCII only; a rule whose heard form differs from the
    /// text in non-ASCII case simply does not fire.
    pub fn rewrite(&self, text: &str) -> String {
        if self.rules.is_empty() {
            return text.to_owned();
        }
        let mut result = text.to_owned();
        for rule in &self.rules {
            if rule.heard.is_empty() {
                continue;
            }
            let mut next = String::with_capacity(result.len());
            let mut offset = 0;
            while offset < result.len() {
                let candidate = result[offset..].char_indices().find_map(|(i, _)| {
                    let start = offset + i;
                    let end = start.checked_add(rule.heard.len())?;
                    let found = result.get(start..end)?;
                    let before = result[..start].chars().next_back();
                    let after = result[end..].chars().next();
                    (found.eq_ignore_ascii_case(&rule.heard)
                        && before.is_none_or(|c| !c.is_alphanumeric())
                        && after.is_none_or(|c| !c.is_alphanumeric()))
                    .then_some((start, end))
                });
                let Some((start, end)) = candidate else {
                    break;
                };
                next.push_str(&result[offset..start]);
                next.push_str(&rule.term);
                offset = end;
            }
            next.push_str(&result[offset..]);
            result = next;
        }
        result
    }
}

/// One term per line. `#` comments and blank lines are skipped, a leading `-`
/// is allowed so the file reads as a markdown list, and a tab-separated number
/// overrides the weight. `heard -> Term` boosts `Term` and also rewrites the
/// phrase the model keeps hearing instead, which is what boosting cannot fix
/// when the model is confident in the wrong word.
fn parse(text: &str) -> (Vec<Term>, Vec<Rule>) {
    let mut terms = Vec::new();
    let mut rules = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("- ").unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (body, weight) = match line.split_once('\t') {
            Some((body, weight)) => (body.trim(), weight.trim().parse::<f32>().unwrap_or(1.0)),
            None => (line, 1.0),
        };
        if !weight.is_finite() || weight <= 0.0 {
            continue;
        }
        let (heard, term) = match body.split_once(" -> ") {
            Some((heard, term)) if !heard.is_empty() && !term.is_empty() => (Some(heard), term),
            _ => (None, body),
        };
        if term.is_empty() {
            continue;
        }
        terms.push(Term {
            text: term.to_owned(),
            weight,
        });
        if let Some(heard) = heard {
            rules.push(Rule {
                heard: heard.to_owned(),
                term: term.to_owned(),
            });
        }
    }
    (terms, rules)
}

/// Trie node, in a flat arena so the active set can be plain indices instead of
/// borrows into the tree we are also mutating.
struct Node {
    children: Vec<(usize, usize)>,
    depth: u32,
    /// The heaviest term running through this node, which is what a bonus for
    /// stepping into it is worth.
    weight: f32,
}

/// A prefix tree over token ids, ready to bias one utterance.
pub struct Boost {
    nodes: Vec<Node>,
    active: Vec<usize>,
    scratch: Vec<(usize, f32)>,
    scale: f32,
    depth_scaling: f32,
    margin: f32,
    /// Terms that could not be spelled out of the model's vocabulary.
    unencodable: Vec<String>,
}

const ROOT: usize = 0;

impl Boost {
    pub fn build(pieces: &Pieces, terms: &[Term]) -> Boost {
        let mut boost = Boost {
            nodes: vec![Node {
                children: Vec::new(),
                depth: 0,
                weight: 0.0,
            }],
            active: Vec::new(),
            scratch: Vec::new(),
            scale: tunable("SEE_COMPUTER_BOOST_SCALE", DEFAULT_SCALE),
            depth_scaling: tunable("SEE_COMPUTER_BOOST_DEPTH", DEFAULT_DEPTH),
            margin: tunable("SEE_COMPUTER_BOOST_MARGIN", DEFAULT_MARGIN),
            unencodable: Vec::new(),
        };
        for term in terms {
            let Some(tokens) = pieces.encode(&term.text) else {
                boost.unencodable.push(term.text.clone());
                continue;
            };
            let mut node = ROOT;
            for token in tokens {
                node = match boost.nodes[node]
                    .children
                    .iter()
                    .find(|(id, _)| *id == token)
                {
                    Some(&(_, child)) => child,
                    None => {
                        let depth = boost.nodes[node].depth + 1;
                        boost.nodes.push(Node {
                            children: Vec::new(),
                            depth,
                            weight: 0.0,
                        });
                        let child = boost.nodes.len() - 1;
                        boost.nodes[node].children.push((token, child));
                        child
                    }
                };
                boost.nodes[node].weight = boost.nodes[node].weight.max(term.weight);
            }
        }
        boost
    }

    /// Whether there is anything to boost. An empty trie should be passed to
    /// the decoder as `None`, not as a hook that does nothing per frame.
    pub fn is_empty(&self) -> bool {
        self.nodes[ROOT].children.is_empty()
    }

    pub fn unencodable(&self) -> &[String] {
        &self.unencodable
    }

    fn child(&self, node: usize, token: usize) -> Option<usize> {
        self.nodes[node]
            .children
            .iter()
            .find_map(|&(id, child)| (id == token).then_some(child))
    }
}

impl TokenBias for Boost {
    fn reset(&mut self) {
        self.active.clear();
    }

    fn bias(&mut self, logits: &mut [f32]) {
        self.scratch.clear();
        // Only tokens the model was already considering are eligible. Without
        // this the bonus is handed to every term's opening token whatever the
        // audio said, so the more terms in the list the likelier one of them
        // wins a step it had no business winning — which is how `pnpm build`
        // became `PNPM Buildship`. Gating on the margin makes the list's size
        // stop mattering: a term the acoustics rule out is never in the running
        // to be boosted at all.
        let best = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let floor = best - self.margin;

        // The root is always live: a term can start at any token.
        for index in 0..=self.active.len() {
            let node = if index == self.active.len() {
                ROOT
            } else {
                self.active[index]
            };
            let depth = self.nodes[node].depth as f32;
            let step = self.scale * (1.0 + self.depth_scaling * depth);
            for &(token, child) in &self.nodes[node].children {
                if token >= logits.len() || logits[token] < floor {
                    continue;
                }
                let bonus = step * self.nodes[child].weight;
                match self.scratch.iter_mut().find(|(id, _)| *id == token) {
                    // Two partial matches can propose the same token; the
                    // stronger claim wins rather than the two stacking up.
                    Some((_, best)) => *best = best.max(bonus),
                    None => self.scratch.push((token, bonus)),
                }
            }
        }
        for &(token, bonus) in &self.scratch {
            logits[token] += bonus;
        }
    }

    fn emitted(&mut self, token: usize) {
        let mut next = Vec::new();
        for index in 0..=self.active.len() {
            let node = if index == self.active.len() {
                ROOT
            } else {
                self.active[index]
            };
            if let Some(child) = self.child(node, token) {
                if !next.contains(&child) {
                    next.push(child);
                }
            }
        }
        // Nothing matched: the term under way was not the one being said, and
        // boosting goes quiet until the next possible start.
        self.active = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A toy vocabulary in the real `vocab.txt` shape: `<piece> <id>`.
    fn pieces() -> Pieces {
        let lines = [
            "\u{2581}Ta 10",
            "uri 11",
            "\u{2581}t 12",
            "ao 13",
            "\u{2581}ry 14",
            "\u{2581}Con 15",
            "vex 16",
            "\u{2581}the 17",
            "<blk> 18",
            "",
            "junk-without-an-id",
        ];
        Pieces::parse(&lines.join("\n")).expect("toy vocabulary")
    }

    fn term(text: &str) -> Term {
        Term {
            text: text.to_owned(),
            weight: 1.0,
        }
    }

    #[test]
    fn merge_keeps_only_hunks_that_gain_a_term() {
        assert_eq!(
            merge("Run PNPM install", "Run pnpm install", &[term("pnpm")]),
            "Run pnpm install"
        );
        assert_eq!(
            merge(
                "Track the signup funnel",
                "Crack the signup funnel",
                &[term("Claude")]
            ),
            "Track the signup funnel"
        );
        assert_eq!(
            merge("same words", "same words", &[term("words")]),
            "same words"
        );
        assert_eq!(merge("", "unrelated words", &[term("Claude")]), "");
        // A changed hunk is selected as a whole when it gains a term.
        assert_eq!(
            merge("context clear your", "Convex Clear Your", &[term("Convex")]),
            "Convex Clear Your"
        );
    }

    #[test]
    fn a_term_is_spelled_out_of_the_models_own_pieces() {
        assert_eq!(pieces().encode("Tauri"), Some(vec![10, 11]));
    }

    #[test]
    fn a_term_the_vocabulary_cannot_spell_is_reported_not_dropped_silently() {
        let boost = Boost::build(&pieces(), &[term("Tauri"), term("Zzzz")]);
        assert_eq!(boost.unencodable(), ["Zzzz"]);
        assert!(!boost.is_empty());
    }

    #[test]
    fn no_terms_means_no_hook() {
        assert!(Boost::build(&pieces(), &[]).is_empty());
    }

    #[test]
    fn only_the_token_a_term_is_waiting_for_is_boosted() {
        let mut boost = Boost::build(&pieces(), &[term("Tauri")]);
        boost.reset();

        let mut logits = vec![0.0; 20];
        boost.bias(&mut logits);
        let opening = logits[10];
        assert!(opening > 0.0, "the first token of a term is nudged");
        assert_eq!(logits[11], 0.0, "the second is not, until the first lands");

        boost.emitted(10);
        let mut logits = vec![0.0; 20];
        boost.bias(&mut logits);
        assert_eq!(
            logits[11], opening,
            "and by default the bonus is flat, so a term the speaker never \
             started stays as cheap to abandon as it was to enter"
        );
    }

    #[test]
    fn the_depth_seam_makes_a_term_harder_to_abandon() {
        let mut boost = Boost::build(&pieces(), &[term("Tauri")]);
        boost.depth_scaling = 2.0;
        boost.reset();

        let mut logits = vec![0.0; 20];
        boost.bias(&mut logits);
        let opening = logits[10];

        boost.emitted(10);
        let mut logits = vec![0.0; 20];
        boost.bias(&mut logits);
        assert!(
            logits[11] > opening,
            "committing costs less once the term is under way: {} vs {opening}",
            logits[11]
        );
    }

    #[test]
    fn a_term_the_audio_rules_out_is_never_in_the_running() {
        let mut boost = Boost::build(&pieces(), &[term("Tauri")]);
        boost.reset();

        let mut logits = vec![0.0; 20];
        logits[17] = 100.0; // the model is certain it heard something else
        boost.bias(&mut logits);
        assert_eq!(
            logits[10], 0.0,
            "a hopeless candidate gets nothing, however long the vocabulary"
        );

        let mut logits = vec![0.0; 20];
        logits[17] = boost.margin / 2.0; // now it is merely ahead, not certain
        boost.bias(&mut logits);
        assert!(logits[10] > 0.0, "a near miss is still worth pushing");
    }

    #[test]
    fn a_broken_match_stops_boosting() {
        let mut boost = Boost::build(&pieces(), &[term("Tauri")]);
        boost.reset();
        boost.emitted(10);
        boost.emitted(17); // "the" — not how the term continues

        let mut logits = vec![0.0; 20];
        boost.bias(&mut logits);
        assert_eq!(logits[11], 0.0, "the tail of a dead term is not boosted");
        assert!(logits[10] > 0.0, "but the term can start again");
    }

    #[test]
    fn overlapping_terms_do_not_stack_their_bonuses() {
        let shared = [term("Tauri"), term("Tauri")];
        let mut one = Boost::build(&pieces(), &shared[..1]);
        let mut two = Boost::build(&pieces(), &shared);
        one.reset();
        two.reset();
        let (mut a, mut b) = (vec![0.0; 20], vec![0.0; 20]);
        one.bias(&mut a);
        two.bias(&mut b);
        assert_eq!(a[10], b[10]);
    }

    #[test]
    fn a_heavier_term_is_pushed_harder() {
        let light = Boost::build(&pieces(), &[term("Convex")]);
        let heavy = Boost::build(
            &pieces(),
            &[Term {
                text: "Convex".to_owned(),
                weight: 3.0,
            }],
        );
        let (mut a, mut b) = (vec![0.0; 20], vec![0.0; 20]);
        let (mut light, mut heavy) = (light, heavy);
        light.bias(&mut a);
        heavy.bias(&mut b);
        assert!(b[15] > a[15]);
    }

    #[test]
    fn the_file_is_a_readable_list() {
        let (parsed, _) = parse(
            "# see.computer vocabulary\n\
             \n\
             - Tauri\n\
             Convex\n\
             pnpm\t4\n\
             \t\n",
        );
        assert_eq!(
            parsed,
            vec![
                term("Tauri"),
                term("Convex"),
                Term {
                    text: "pnpm".to_owned(),
                    weight: 4.0
                },
            ]
        );
    }

    #[test]
    fn rules_and_learned_file() {
        let (terms, rules) = parse("- superbase -> Supabase\t2\n");
        assert_eq!(
            terms,
            vec![Term {
                text: "Supabase".into(),
                weight: 2.0
            }]
        );
        assert_eq!(
            rules,
            vec![Rule {
                heard: "superbase".into(),
                term: "Supabase".into()
            }]
        );
        let root = std::env::temp_dir().join(format!("lexicon-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("vocabulary.md"), "- Supabase\n- Tauri\n").unwrap();
        fs::write(
            root.join("learned.md"),
            "- superbase -> Supabase\t2\n- potato mode -> poteto mode\n",
        )
        .unwrap();
        let lexicon = Lexicon::load(root.join("vocabulary.md"));
        assert_eq!(
            lexicon.terms(),
            [term("Supabase"), term("Tauri"), term("poteto mode")].map(|mut t| {
                if t.text == "Supabase" {
                    t.weight = 2.0;
                }
                t
            })
        );
        assert_eq!(
            lexicon.rewrite("I use Superbase, superbase and superbases"),
            "I use Supabase, Supabase and superbases"
        );
        assert_eq!(
            lexicon.rewrite("potato mode or potato  mode"),
            "poteto mode or potato  mode"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_file_is_an_empty_vocabulary_not_a_failure() {
        let path = std::env::temp_dir().join("see-computer-absent-vocabulary.md");
        let _ = fs::remove_file(&path);
        assert!(Lexicon::load(path).terms().is_empty());
    }

    #[test]
    fn edits_to_the_file_are_picked_up() {
        let path = std::env::temp_dir().join(format!("vocab-{}.md", std::process::id()));
        fs::write(&path, "Tauri\n").unwrap();
        let mut lexicon = Lexicon::load(path.clone());
        assert_eq!(lexicon.terms(), [term("Tauri")]);

        // Coarse mtimes on some filesystems would hide a same-second rewrite.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&path, "Tauri\nConvex\n").unwrap();
        assert!(lexicon.refresh());
        assert_eq!(lexicon.terms(), [term("Tauri"), term("Convex")]);
        assert!(!lexicon.refresh(), "an untouched file is not re-read");
        let _ = fs::remove_file(&path);
    }
}
