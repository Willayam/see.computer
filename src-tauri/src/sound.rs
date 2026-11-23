//! How alike two words sound, shared by the learner and the decoder's
//! name rescoring, so the iPhone engine can compile it too.

/// Letters as they are said: lowercase, digits spelled out, the rest dropped.
pub(crate) fn spoken(text: &str) -> Vec<char> {
    const DIGITS: [&str; 10] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ];
    let mut out = Vec::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        match c.to_digit(10) {
            Some(d) => out.extend(DIGITS[d as usize].chars()),
            None if c.is_alphabetic() => out.push(c),
            None => {}
        }
    }
    out
}

pub(crate) fn distance(a: &[char], b: &[char], gap: f32, swap: impl Fn(char, char) -> f32) -> f32 {
    let mut previous: Vec<f32> = (0..=b.len()).map(|j| j as f32 * gap).collect();
    for (i, x) in a.iter().enumerate() {
        let mut current = vec![(i + 1) as f32 * gap];
        for (j, y) in b.iter().enumerate() {
            current.push(
                (previous[j + 1] + gap)
                    .min(current[j] + gap)
                    .min(previous[j] + swap(*x, *y)),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

/// Sound class bitmask for `sound_cost`: bit 0 = vowel, bits 1..=8 = neighbour groups.
/// `'c'` belongs to both `"gkcq"` and `"szcx"`, matching the original group table.
pub(crate) fn sound_class(c: char) -> u16 {
    const ASCII: [u16; 128] = {
        let mut t = [0u16; 128];
        t[b'a' as usize] = 1 << 0;
        t[b'e' as usize] = 1 << 0;
        t[b'i' as usize] = 1 << 0;
        t[b'o' as usize] = 1 << 0;
        t[b'u' as usize] = 1 << 0;
        t[b'y' as usize] = (1 << 0) | (1 << 8);
        t[b'b' as usize] = 1 << 1;
        t[b'p' as usize] = 1 << 1;
        t[b'd' as usize] = 1 << 2;
        t[b't' as usize] = 1 << 2;
        t[b'g' as usize] = 1 << 3;
        t[b'k' as usize] = 1 << 3;
        t[b'c' as usize] = (1 << 3) | (1 << 5);
        t[b'q' as usize] = 1 << 3;
        t[b'f' as usize] = 1 << 4;
        t[b'v' as usize] = 1 << 4;
        t[b'w' as usize] = 1 << 4;
        t[b's' as usize] = 1 << 5;
        t[b'z' as usize] = 1 << 5;
        t[b'x' as usize] = 1 << 5;
        t[b'm' as usize] = 1 << 6;
        t[b'n' as usize] = 1 << 6;
        t[b'l' as usize] = 1 << 7;
        t[b'r' as usize] = 1 << 7;
        t[b'j' as usize] = 1 << 8;
        t
    };
    let code = c as u32;
    if code < 128 {
        ASCII[code as usize]
    } else if matches!(c, 'å' | 'ä' | 'ö') {
        1 << 0
    } else {
        0
    }
}

#[inline]
fn sound_swap_x4(x: char, cx: u16, y: char, cy: u16) -> u32 {
    if x == y {
        0
    } else {
        let shared = cx & cy;
        if shared & 1 != 0 {
            1
        } else if shared != 0 {
            2
        } else {
            4
        }
    }
}

/// Exact `sound_cost(a, b) <= limit`, computed in quarter-unit integers with a
/// tight banded DP and zero heap allocations for strings up to 64 chars.
pub(crate) fn sound_cost_within(
    a: &[char],
    a_cls: &[u16],
    b: &[char],
    b_cls: &[u16],
    limit: f32,
) -> bool {
    if limit < 0.0 {
        return false;
    }
    let max_x4 = (limit * 4.0).floor() as u32;
    let la = a.len();
    let lb = b.len();
    let diff = la.abs_diff(lb);
    let min_gaps = (diff as u32).saturating_mul(3);
    if min_gaps > max_x4 {
        return false;
    }
    if la == 0 || lb == 0 {
        return true;
    }
    let slack = ((max_x4 - min_gaps) / 6) as usize;
    let (lo_off, hi_off) = if la >= lb {
        (diff + slack, slack)
    } else {
        (slack, diff + slack)
    };
    const INF: u32 = u32::MAX / 4;
    let mut stack = [INF; 65];
    let mut heap = Vec::new();
    let row: &mut [u32] = if lb < stack.len() {
        &mut stack[..=lb]
    } else {
        heap.resize(lb + 1, INF);
        &mut heap
    };
    let init_hi = hi_off.min(lb);
    for (j, cell) in row.iter_mut().enumerate().take(init_hi + 1) {
        *cell = (j as u32) * 3;
    }
    for i in 1..=la {
        let j_lo = i.saturating_sub(lo_off).max(1);
        let j_hi = (i + hi_off).min(lb);
        let x = a[i - 1];
        let cx = a_cls[i - 1];
        let mut prev_diag = if j_lo == 1 {
            let old_zero = row[0];
            row[0] = (i as u32) * 3;
            old_zero
        } else {
            row[j_lo - 1]
        };
        if j_lo > 1 {
            row[j_lo - 1] = INF;
        }
        let mut row_min = if j_lo == 1 { row[0] } else { INF };
        for j in j_lo..=j_hi {
            let up = row[j];
            let left = row[j - 1];
            let sub = prev_diag + sound_swap_x4(x, cx, b[j - 1], b_cls[j - 1]);
            let best = (up + 3).min(left + 3).min(sub);
            prev_diag = up;
            row[j] = best;
            row_min = row_min.min(best);
        }
        if j_hi < lb {
            row[j_hi + 1] = INF;
        }
        if row_min > max_x4 {
            return false;
        }
    }
    row[lb] <= max_x4
}

pub(crate) fn sound_cost(a: &[char], b: &[char]) -> f32 {
    const VOWELS: &str = "aeiouyåäö";
    const NEIGHBOURS: [&str; 8] = ["bp", "dt", "gkcq", "fvw", "szcx", "mn", "lr", "jy"];
    distance(a, b, 0.75, |x, y| {
        if x == y {
            0.0
        } else if VOWELS.contains(x) && VOWELS.contains(y) {
            0.25
        } else if NEIGHBOURS
            .iter()
            .any(|group| group.contains(x) && group.contains(y))
        {
            0.5
        } else {
            1.0
        }
    })
}
