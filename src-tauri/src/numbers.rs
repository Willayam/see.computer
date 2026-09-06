//! Spoken numbers written the way they are read.
//!
//! Parakeet decides on its own whether `twenty percent` comes out as words or
//! as `20%`, and over a week of dictation it was a coin flip either way. This
//! pass makes the choice once, after the model, with a grammar rather than a
//! word list: a lexer for the thirty-odd number words of English and Swedish,
//! a parser that composes them into a value, and one style rule for writing it
//! down. Ten and above become digits. Zero to nine stay words unless they carry
//! a unit or a decimal point, which keeps `one thing` and the Swedish article
//! `en` as the words they nearly always are.
//!
//! Parakeet often writes the digits itself and leaves a seam where its
//! tokenizer split them, `20 %` and `5 .1 ,`, so punctuation that belongs to
//! the digits before it is pulled back on.
//!
//! Idempotent: digits already in the text pass through, so a model that learns
//! to do this itself makes the pass a no-op rather than a conflict.

/// Rewrites every spoken number the style rule says to write in digits.
pub fn normalise(text: &str) -> String {
    let words: Vec<Word> = words(text).map(Word::parse).collect();
    let lang = tongue(&words);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < words.len() {
        let word = &words[at];
        if at == 0 || !glues(&out, word, lang) {
            out.push_str(word.space);
        }
        out.push_str(word.lead);
        if let Some((number, used)) = number_at(&words[at..], lang) {
            out.push_str(&number.written());
            out.push_str(words[at + used - 1].trail);
            at += used;
        } else {
            out.push_str(word.core);
            out.push_str(word.trail);
            at += 1;
        }
    }
    out
}

/// Whether this word's leading punctuation belongs to the digits before it.
/// The model's tokenizer writes `235,000` as `235 ,000` and `20%` as `20 %`,
/// and a sentence's own full stop can come away from the figure it follows.
fn glues(out: &str, word: &Word, lang: Lang) -> bool {
    if !out.ends_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    // Only an ordinary space is a seam. The no-break space this pass writes
    // into Swedish figures is deliberate, and re-reading must not close it.
    if word.space != " " {
        return false;
    }
    let digits = word.core.is_empty() || word.core.bytes().all(|b| b.is_ascii_digit());
    match word.lead.chars().next() {
        // Swedish keeps its space, so there is nothing to close up.
        Some('%') => lang == Lang::En && word.core.is_empty(),
        Some('.' | ',') => digits && word.lead.len() == 1,
        _ => false,
    }
}

/// Which language the numbers in this text are being read in, decided by the
/// number words present. `sex` and `en` do not get a vote, because both are
/// English words too. Digits alone say nothing either way, so a text whose
/// figures are already written reads as English.
fn tongue(words: &[Word]) -> Lang {
    let swedish = words.iter().any(|word| {
        let lower = word.core.to_lowercase();
        !matches!(lower.as_str(), "en" | "sex")
            && lex(&lower, Lang::Sv).is_some()
            && lex(&lower, Lang::En).is_none()
    });
    if swedish {
        Lang::Sv
    } else {
        Lang::En
    }
}

/// A word with the whitespace before it and the punctuation either side kept
/// apart, so everything the pass does not rewrite comes back untouched.
struct Word<'a> {
    space: &'a str,
    lead: &'a str,
    core: &'a str,
    trail: &'a str,
}

impl<'a> Word<'a> {
    fn parse((space, word): (&'a str, &'a str)) -> Word<'a> {
        let start = word.find(char::is_alphanumeric).unwrap_or(word.len());
        let (lead, rest) = word.split_at(start);
        let end = rest
            .rfind(char::is_alphanumeric)
            .map(|index| index + rest[index..].chars().next().map_or(0, char::len_utf8))
            .unwrap_or(0);
        let (core, trail) = rest.split_at(end);
        Word {
            space,
            lead,
            core,
            trail,
        }
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    En,
    Sv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    /// `zero` to `nine`.
    Unit(u64),
    /// `ten` to `nineteen`.
    Teen(u64),
    /// `twenty` to `ninety`.
    Tens(u64),
    Hundred,
    /// `thousand`, `million`, `billion`.
    Scale(u64),
    /// Already written, as in `50 percent`.
    Digits(u64),
    And,
    /// `a hundred`, `a million`.
    A,
    /// `five point one`, `fem komma en`.
    Point,
    Percent,
}

use Piece::*;

const MILLION: u64 = 1_000_000;
const BILLION: u64 = 1_000_000_000;

fn english(word: &str) -> Option<Piece> {
    Some(match word {
        "zero" => Unit(0),
        "one" => Unit(1),
        "two" => Unit(2),
        "three" => Unit(3),
        "four" => Unit(4),
        "five" => Unit(5),
        "six" => Unit(6),
        "seven" => Unit(7),
        "eight" => Unit(8),
        "nine" => Unit(9),
        "ten" => Teen(10),
        "eleven" => Teen(11),
        "twelve" => Teen(12),
        "thirteen" => Teen(13),
        "fourteen" => Teen(14),
        "fifteen" => Teen(15),
        "sixteen" => Teen(16),
        "seventeen" => Teen(17),
        "eighteen" => Teen(18),
        "nineteen" => Teen(19),
        "twenty" => Tens(20),
        "thirty" => Tens(30),
        "forty" => Tens(40),
        "fifty" => Tens(50),
        "sixty" => Tens(60),
        "seventy" => Tens(70),
        "eighty" => Tens(80),
        "ninety" => Tens(90),
        "hundred" => Hundred,
        "thousand" => Scale(1_000),
        "million" => Scale(MILLION),
        "billion" => Scale(BILLION),
        "and" => And,
        "a" => A,
        "point" => Point,
        "percent" => Percent,
        _ => return None,
    })
}

/// Swedish writes a number as one word, `tvåhundrasjuttionio`, so a word is
/// read as the longest run of number words that consumes all of it.
const SWEDISH: &[(&str, Piece)] = &[
    ("noll", Unit(0)),
    ("en", Unit(1)),
    ("ett", Unit(1)),
    ("två", Unit(2)),
    ("tre", Unit(3)),
    ("fyra", Unit(4)),
    ("fem", Unit(5)),
    ("sex", Unit(6)),
    ("sju", Unit(7)),
    ("åtta", Unit(8)),
    ("nio", Unit(9)),
    ("tio", Teen(10)),
    ("elva", Teen(11)),
    ("tolv", Teen(12)),
    ("tretton", Teen(13)),
    ("fjorton", Teen(14)),
    ("femton", Teen(15)),
    ("sexton", Teen(16)),
    ("sjutton", Teen(17)),
    ("arton", Teen(18)),
    ("nitton", Teen(19)),
    ("tjugo", Tens(20)),
    ("trettio", Tens(30)),
    ("fyrtio", Tens(40)),
    ("femtio", Tens(50)),
    ("sextio", Tens(60)),
    ("sjuttio", Tens(70)),
    ("åttio", Tens(80)),
    ("nittio", Tens(90)),
    ("hundra", Hundred),
    ("tusen", Scale(1_000)),
    ("miljoner", Scale(MILLION)),
    ("miljon", Scale(MILLION)),
    ("miljarder", Scale(BILLION)),
    ("miljard", Scale(BILLION)),
    ("komma", Point),
    ("procent", Percent),
];

fn swedish(word: &str) -> Option<Vec<Piece>> {
    let mut rest = word;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let (prefix, piece) = SWEDISH
            .iter()
            .filter(|(prefix, _)| rest.starts_with(prefix))
            .max_by_key(|(prefix, _)| prefix.len())?;
        out.push(*piece);
        rest = &rest[prefix.len()..];
    }
    (!out.is_empty()).then_some(out)
}

/// The pieces one word contributes, or none if any part of it is not a number
/// word: `twenty-five` reads, `twenty-something` does not.
fn lex(core: &str, lang: Lang) -> Option<Vec<Piece>> {
    if !core.is_empty() && core.bytes().all(|b| b.is_ascii_digit()) {
        return core.parse().ok().map(|n| vec![Digits(n)]);
    }
    let lower = core.to_lowercase();
    let mut out = Vec::new();
    for part in lower.split('-') {
        match lang {
            Lang::En => out.push(english(part)?),
            Lang::Sv => out.extend(swedish(part)?),
        }
    }
    Some(out)
}

/// The language a number word is read in, or none for a word that says
/// nothing about it, which is any figure already written in digits.
fn language(core: &str) -> Option<Lang> {
    if core.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    [Lang::En, Lang::Sv]
        .into_iter()
        .find(|lang| lex(core, *lang).is_some())
}

struct Number {
    lang: Lang,
    value: u64,
    /// Digits after the point, kept as said so `five point one four` is `5.14`.
    fraction: Option<String>,
    /// `2 million` keeps its word; `2,000,000` is not how anyone writes it.
    magnitude: u64,
    percent: bool,
}

impl Number {
    fn written(&self) -> String {
        let mut out = grouped(self.value, self.lang);
        if let Some(fraction) = &self.fraction {
            out.push(match self.lang {
                Lang::En => '.',
                Lang::Sv => ',',
            });
            out.push_str(fraction);
        }
        if self.magnitude > 1 {
            out.push(' ');
            out.push_str(match (self.lang, self.magnitude, self.value) {
                (Lang::En, MILLION, _) => "million",
                (Lang::En, _, _) => "billion",
                (Lang::Sv, MILLION, 1) => "miljon",
                (Lang::Sv, MILLION, _) => "miljoner",
                (Lang::Sv, _, 1) => "miljard",
                (Lang::Sv, _, _) => "miljarder",
            });
        }
        if self.percent {
            out.push_str(match self.lang {
                Lang::En => "%",
                Lang::Sv => "\u{a0}%",
            });
        }
        out
    }
}

/// Thousands are separated from ten thousand up. Four digits are left alone
/// because half of them are years.
fn grouped(value: u64, lang: Lang) -> String {
    let digits = value.to_string();
    if value < 10_000 {
        return digits;
    }
    let separator = match lang {
        Lang::En => ',',
        Lang::Sv => '\u{a0}',
    };
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(separator);
        }
        out.push(digit);
    }
    out
}

/// The number starting at the first word, if the style rule wants it written
/// in digits, with how many words it took.
fn number_at(words: &[Word], text: Lang) -> Option<(Number, usize)> {
    let lang = match language(words[0].core) {
        Some(lang) => lang,
        // A figure takes the reading of the text around it: `50 procent`.
        None if lex(words[0].core, text).is_some() => text,
        None => return None,
    };
    let mut pieces = Vec::new();
    let mut word_of = Vec::new();
    for (index, word) in words.iter().enumerate() {
        // Punctuation closes a number: `twenty, thirty` is two of them.
        if index > 0 && !word.lead.is_empty() {
            break;
        }
        let Some(read) = lex(word.core, lang) else {
            break;
        };
        word_of.extend(std::iter::repeat_n(index, read.len()));
        pieces.extend(read);
        if !word.trail.is_empty() {
            break;
        }
    }
    let (number, used) = parse(&pieces, lang)?;
    // A number may not stop partway through a word.
    if used < pieces.len() && word_of[used] == word_of[used - 1] {
        return None;
    }
    let plain = number.fraction.is_none() && number.magnitude == 1 && !number.percent;
    if plain && (number.value < 10 || matches!(pieces[..used], [Digits(_)])) {
        return None;
    }
    Some((number, word_of[used - 1] + 1))
}

fn parse(pieces: &[Piece], lang: Lang) -> Option<(Number, usize)> {
    let mut at = 0;
    let mut total = 0u64;
    let mut last_scale = u64::MAX;
    let mut number = Number {
        lang,
        value: 0,
        fraction: None,
        magnitude: 1,
        percent: false,
    };
    loop {
        let (small, used) = parse_small(&pieces[at..])?;
        at += used;
        let fraction = decimal(&pieces[at..]);
        if let Some(fraction) = &fraction {
            at += 1 + fraction.len();
        }
        match pieces.get(at) {
            Some(&Scale(scale)) if scale < last_scale && fraction.is_none() => {
                at += 1;
                total = total.saturating_add(small.saturating_mul(scale));
                last_scale = scale;
                if pieces.get(at) == Some(&And) && parse_small(&pieces[at + 1..]).is_some() {
                    at += 1;
                }
                if parse_small(&pieces[at..]).is_none() {
                    break;
                }
            }
            Some(&Scale(scale)) if total == 0 && scale >= MILLION => {
                // `two point five million`
                at += 1;
                number.value = small;
                number.fraction = fraction;
                number.magnitude = scale;
                break;
            }
            _ => {
                number.value = total.saturating_add(small);
                number.fraction = fraction;
                break;
            }
        }
    }
    if number.value == 0 {
        number.value = total;
    }
    if number.fraction.is_none() && number.magnitude == 1 {
        for magnitude in [BILLION, MILLION] {
            if number.value >= magnitude && number.value % magnitude == 0 {
                number.value /= magnitude;
                number.magnitude = magnitude;
                break;
            }
        }
    }
    if pieces.get(at) == Some(&Percent) {
        number.percent = true;
        at += 1;
    }
    Some((number, at))
}

/// Everything below a thousand: `two hundred and seventy nine`, `a hundred`,
/// `nineteen ninety nine`, `seven fifty`.
fn parse_small(pieces: &[Piece]) -> Option<(u64, usize)> {
    let (mut value, mut at) = match pieces {
        [Digits(n), ..] => return Some((*n, 1)),
        [A, Hundred, ..] => (1, 1),
        [A, Scale(_), ..] => return Some((1, 1)),
        [Hundred, ..] => (1, 0),
        _ => group(pieces)?,
    };
    if pieces.get(at) == Some(&Hundred) {
        at += 1;
        value *= 100;
        if pieces.get(at) == Some(&And) && group(&pieces[at + 1..]).is_some() {
            at += 1;
        }
        if let Some((rest, used)) = group(&pieces[at..]) {
            value += rest;
            at += used;
        }
    } else if (10..100).contains(&value) && matches!(pieces.first(), Some(Teen(_) | Tens(_))) {
        // Years come in pairs: `twenty twenty-five`, `nineteen ninety`.
        if let Some((tail, used)) = group(&pieces[at..]) {
            if matches!(pieces[at], Teen(_) | Tens(_)) {
                value = value * 100 + tail;
                at += used;
            }
        }
    }
    Some((value, at))
}

/// Below a hundred, plus the spoken shortcut that drops it: `seven fifty`.
fn group(pieces: &[Piece]) -> Option<(u64, usize)> {
    Some(match pieces {
        [Tens(t), Unit(u), ..] => (t + u, 2),
        [Tens(t), ..] | [Teen(t), ..] => (*t, 1),
        [Unit(h), Tens(t), Unit(u), ..] => (h * 100 + t + u, 3),
        [Unit(h), Tens(t), ..] => (h * 100 + t, 2),
        [Unit(u), ..] => (*u, 1),
        _ => return None,
    })
}

/// The digits after `point`, read one at a time.
fn decimal(pieces: &[Piece]) -> Option<String> {
    let [Point, rest @ ..] = pieces else {
        return None;
    };
    let digits: String = rest
        .iter()
        .map_while(|piece| match piece {
            Unit(d) | Digits(d) if *d < 10 => char::from_digit(*d as u32, 10),
            _ => None,
        })
        .collect();
    (!digits.is_empty()).then_some(digits)
}

#[cfg(test)]
mod tests {
    use super::normalise;

    #[track_caller]
    fn check(said: &str, written: &str) {
        assert_eq!(normalise(said), written);
        assert_eq!(normalise(written), written, "not idempotent");
    }

    #[test]
    fn a_percentage_is_a_figure_and_a_sign() {
        check(
            "How could we cut it to twenty percent of what it is right now?",
            "How could we cut it to 20% of what it is right now?",
        );
        check(
            "What if we just have a hundred percent tint?",
            "What if we just have 100% tint?",
        );
        check("All hundred percent.", "All 100%.");
        check(
            "let's add seventy and fifty percent to the test",
            "let's add 70 and 50% to the test",
        );
    }

    #[test]
    fn composes_the_long_ones() {
        check(
            "the two hundred and seventy nine review ones",
            "the 279 review ones",
        );
        check(
            "Over twenty-five thousand hand drawn assets",
            "Over 25,000 hand drawn assets",
        );
        check(
            "the ADX startup growth six thousand thing",
            "the ADX startup growth 6000 thing",
        );
        check("two hundred thousand rows", "200,000 rows");
        check("one hundred and twenty", "120");
    }

    #[test]
    fn a_decimal_is_read_digit_by_digit() {
        check(
            "we now have Fable Five point one, not just Fable.",
            "we now have Fable 5.1, not just Fable.",
        );
        check("five point one four", "5.14");
        check("zero point five", "0.5");
    }

    #[test]
    fn millions_keep_their_word() {
        check("the two million, the seven K", "the 2 million, the seven K");
        check("two point five million users", "2.5 million users");
        check("a million", "1 million");
        check("two million three hundred thousand", "2,300,000");
    }

    #[test]
    fn small_numbers_stay_words_unless_they_carry_a_unit() {
        check(
            "One thing I wonder is like the A formula",
            "One thing I wonder is like the A formula",
        );
        check("two of them", "two of them");
        check("one percent", "1%");
        check("zero", "zero");
    }

    #[test]
    fn ten_and_up_become_digits() {
        check(
            "Please implement V sixty one into the",
            "Please implement V 61 into the",
        );
        check("iOS twenty seven or whatever", "iOS 27 or whatever");
        check(
            "This is still active twenty four to twenty six.",
            "This is still active 24 to 26.",
        );
        check("Ten prototypes", "10 prototypes");
    }

    #[test]
    fn years_come_in_pairs() {
        check("in twenty twenty-five", "in 2025");
        check("nineteen ninety nine", "1999");
        check("twenty twenty", "2020");
    }

    #[test]
    fn the_spoken_shortcut_for_hundreds() {
        check("the seven fifty Claude Wax", "the 750 Claude Wax");
    }

    #[test]
    fn punctuation_closes_a_number_and_stays_where_it_was() {
        check("twenty, thirty percent", "20, 30%");
        check("(twenty five)", "(25)");
        check("Twenty percent.", "20%.");
    }

    #[test]
    fn a_number_already_in_digits_only_gains_its_unit() {
        check("50 percent of viewers", "50% of viewers");
        check("25 thousand", "25,000");
        check(
            "Does 3319 make 3318 unnecessary?",
            "Does 3319 make 3318 unnecessary?",
        );
        check("from 2025 to 2026", "from 2025 to 2026");
    }

    #[test]
    fn closes_the_seams_the_model_leaves() {
        check(
            "How could we cut it to 20 % of what it is right now?",
            "How could we cut it to 20% of what it is right now?",
        );
        check(
            "We now have Fable 5 .1 , not just Fable.",
            "We now have Fable 5.1, not just Fable.",
        );
        check("50 procent av dem", "50\u{a0}% av dem");
        check(
            "It ended at 2026 . Then we stopped.",
            "It ended at 2026. Then we stopped.",
        );
    }

    #[test]
    fn repairs_the_models_split_thousands() {
        check(
            "How is it 235 ,000 rows? Well we have 25 ,000 rows",
            "How is it 235,000 rows? Well we have 25,000 rows",
        );
    }

    #[test]
    fn swedish_numbers_are_one_word() {
        check("tvåhundrasjuttionio rader", "279 rader");
        check("tjugofem tusen", "25\u{a0}000");
        check("tjugo procent", "20\u{a0}%");
        check("inte riktigt hundra procent", "inte riktigt 100\u{a0}%");
        check("fem komma en", "5,1");
        check("två miljoner", "2 miljoner");
        check("en miljon", "1 miljon");
    }

    #[test]
    fn swedish_articles_are_not_numbers() {
        check("en bil och ett hus", "en bil och ett hus");
        check("sex av dem", "sex av dem");
        check("entré", "entré");
    }

    #[test]
    fn a_number_word_glued_to_another_word_is_left_alone() {
        check("twenty-something", "twenty-something");
        check("thousands of them", "thousands of them");
        check("a point I made", "a point I made");
        check("ten point", "10 point");
    }

    #[test]
    fn keeps_the_spacing_it_did_not_touch() {
        check(
            "first line\nsecond  twenty line",
            "first line\nsecond  20 line",
        );
    }

    #[test]
    fn leaves_clean_speech_untouched() {
        let said = "Push the words you keep saying into the decoder.";
        check(said, said);
    }
}
