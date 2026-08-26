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

/// The terms the user cares about, and where they were read from.
///
/// A plain file in the same folder as the dictation history, in keeping with
/// the rest of the app: something to open and edit, not a settings pane.
pub struct Lexicon {
    path: PathBuf,
    stamp: Option<SystemTime>,
    terms: Vec<Term>,
}

impl Lexicon {
    /// `~/Documents/see.computer/vocabulary.md`, beside the dictation history,
    /// unless `SEE_COMPUTER_VOCABULARY` names a file directly.
    ///
    /// Falls back to the history's own fallback location when Documents holds
    /// no vocabulary but Application Support does, so a machine that denied
    /// Documents access keeps working the way its history already does.
    pub fn default_path() -> PathBuf {
        if let Some(path) = std::env::var_os("SEE_COMPUTER_VOCABULARY") {
            return PathBuf::from(path);
        }
        const NAME: &str = "vocabulary.md";
        let documents = dirs::document_dir()
            .or_else(dirs::home_dir)
            .map(|dir| dir.join("see.computer").join(NAME));
        let support =
            dirs::data_dir().map(|dir| dir.join("see.computer").join("history").join(NAME));
        match (documents, support) {
            (Some(documents), Some(support)) if !documents.exists() && support.exists() => support,
            (Some(documents), _) => documents,
            (None, Some(support)) => support,
            (None, None) => PathBuf::from(NAME),
        }
    }

    pub fn load(path: PathBuf) -> Lexicon {
        let mut lexicon = Lexicon {
            path,
            stamp: None,
            terms: Vec::new(),
        };
        lexicon.refresh();
        lexicon
    }

    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    /// Re-read the file when it has changed on disk. Returns whether the terms
    /// moved, so a caller can avoid rebuilding a trie that would come out the
    /// same. A missing file is not an error; it means no custom vocabulary yet.
    pub fn refresh(&mut self) -> bool {
        let stamp = fs::metadata(&self.path)
            .and_then(|meta| meta.modified())
            .ok();
        if stamp == self.stamp && (stamp.is_some() || self.terms.is_empty()) {
            return false;
        }
        self.stamp = stamp;
        let parsed = fs::read_to_string(&self.path)
            .map(|text| parse(&text))
            .unwrap_or_default();
        let changed = parsed != self.terms;
        self.terms = parsed;
        changed
    }
}

/// One term per line. `#` comments and blank lines are skipped, a leading `-`
/// is allowed so the file reads as a markdown list, and a tab-separated number
/// overrides the weight — which is how the correction capture will record that
/// a term has been confirmed several times.
fn parse(text: &str) -> Vec<Term> {
    let mut terms = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("- ").unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (text, weight) = match line.split_once('\t') {
            Some((text, weight)) => (
                text.trim(),
                weight.trim().parse::<f32>().ok().unwrap_or(1.0),
            ),
            None => (line, 1.0),
        };
        if text.is_empty() || !weight.is_finite() || weight <= 0.0 {
            continue;
        }
        terms.push(Term {
            text: text.to_owned(),
            weight,
        });
    }
    terms
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
        let parsed = parse(
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
