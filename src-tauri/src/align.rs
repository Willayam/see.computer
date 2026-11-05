//! Word alignment between two renderings of the same speech. Shared with the
//! iPhone app, so it stays free of anything Mac-only.

pub(crate) fn strip(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}

fn form(token: &str) -> String {
    strip(token).to_lowercase()
}

pub(crate) fn alignment(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let af: Vec<_> = a.iter().map(|s| form(s)).collect();
    let bf: Vec<_> = b.iter().map(|s| form(s)).collect();
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            dp[i][j] = if af[i] == bf[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut anchors = Vec::new();
    while i < a.len() && j < b.len() {
        if af[i] == bf[j] {
            anchors.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    anchors
}
