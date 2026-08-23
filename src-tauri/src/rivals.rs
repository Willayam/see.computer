use std::collections::HashSet;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::pill::{Notice, PillEvent};

const RIVALS: &[(&str, &str)] = &[
    ("Hex.app", "Hex"),
    ("Wispr Flow.app", "Wispr Flow"),
    ("superwhisper.app", "Superwhisper"),
    ("MacWhisper.app", "MacWhisper"),
    ("VoiceInk.app", "VoiceInk"),
    ("Aqua Voice.app", "Aqua Voice"),
    ("Handy.app", "Handy"),
];

fn detect(process_paths: &[String]) -> Vec<&'static str> {
    RIVALS
        .iter()
        .filter_map(|(bundle, name)| {
            process_paths
                .iter()
                .any(|path| contains_bundle(path, bundle))
                .then_some(*name)
        })
        .collect()
}

fn contains_bundle(path: &str, bundle: &str) -> bool {
    path.match_indices('/').any(|(slash, _)| {
        let remainder = &path[slash + 1..];
        let Some(candidate) = remainder.get(..bundle.len()) else {
            return false;
        };
        candidate.eq_ignore_ascii_case(bundle)
            && remainder
                .get(bundle.len()..)
                .is_some_and(|suffix| suffix.starts_with("/Contents/MacOS/"))
    })
}

fn running_process_paths() -> Vec<String> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-axo", "comm="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

pub fn spawn(pill: Sender<PillEvent>) {
    std::thread::spawn(move || {
        let mut previous = HashSet::new();
        loop {
            let current = detect(&running_process_paths());
            for name in current
                .iter()
                .copied()
                .filter(|name| !previous.contains(name))
            {
                if pill
                    .send(PillEvent::Flash(Notice::RivalDictation(name.to_owned())))
                    .is_err()
                {
                    return;
                }
                eprintln!("rival dictation app running: {name}");
            }
            previous = current.into_iter().collect();
            std::thread::sleep(Duration::from_secs(10));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::detect;

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn detects_rival_bundle_path() {
        assert_eq!(
            detect(&paths(&["/Applications/Hex.app/Contents/MacOS/Hex"])),
            vec!["Hex"]
        );
    }

    #[test]
    fn detects_bundle_case_insensitively() {
        assert_eq!(
            detect(&paths(&[
                "/Applications/hEx.ApP/Contents/MacOS/renamed-executable"
            ])),
            vec!["Hex"]
        );
    }

    #[test]
    fn rejects_unrelated_process_paths() {
        assert!(detect(&paths(&[
            "/Applications/HexFriend.app/Contents/MacOS/HexFriend",
            "/usr/local/bin/hexdump",
            "/Applications/see.computer.app/Contents/MacOS/see-computer",
        ]))
        .is_empty());
    }

    #[test]
    fn detects_multiple_rivals() {
        assert_eq!(
            detect(&paths(&[
                "/Applications/Handy.app/Contents/MacOS/Handy",
                "/Applications/Wispr Flow.app/Contents/MacOS/Wispr Flow",
                "/Applications/Hex.app/Contents/MacOS/Hex",
            ])),
            vec!["Hex", "Wispr Flow", "Handy"]
        );
    }
}
