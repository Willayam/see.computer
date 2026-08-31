//! Clip packaging tests.

use super::*;
use crate::text::Text;

fn segment(start_ms: u64, end_ms: u64, text: &str) -> Segment {
    Segment {
        start_ms,
        end_ms,
        text: text.to_owned(),
    }
}

#[test]
fn frame_times_follow_sentences_with_a_minimum_gap() {
    let segments = [
        segment(400, 3_900, "a"),
        segment(4_000, 7_900, "b"),
        segment(5_000, 7_000, "c"),
        segment(8_000, 9_900, "d"),
    ];
    assert_eq!(frame_times(10_220, &segments), vec![0, 4_000, 8_000]);
}

#[test]
fn frame_times_sample_quarters_without_speech() {
    assert_eq!(frame_times(40_000, &[]), vec![0, 10_000, 20_000, 30_000]);
    assert_eq!(frame_times(0, &[]), vec![0]);
}

#[test]
fn frame_times_clamp_to_the_clip_and_cap_the_count() {
    let segments: Vec<Segment> = (0..40)
        .map(|index| segment(index * 4_000, index * 4_000 + 1_000, "x"))
        .collect();
    let times = frame_times(30_000, &segments);
    assert!(times.len() <= MAX_FRAMES);
    assert!(times.iter().all(|at| *at <= 29_900));
}

#[test]
fn paste_names_each_capture_combination() {
    let markdown = Path::new("/tmp/demo/take.md");
    let cases = [
        (1, 0, 0, "1 screenshot"),
        (2, 0, 0, "2 screenshots"),
        (0, 1, 2_000, "1 clip (0:02)"),
        (0, 2, 47_000, "2 clips (0:47)"),
        (2, 1, 2_000, "2 screenshots, 1 clip (0:02)"),
        (3, 2, 72_000, "3 screenshots, 2 clips (1:12)"),
    ];
    for (screenshots, clips, duration_ms, tail) in cases {
        assert_eq!(
            paste(
                Some("Look at the misaligned button."),
                screenshots,
                clips,
                duration_ms,
                markdown,
            ),
            format!("\"Look at the misaligned button.\"\n\n{tail}: /tmp/demo/take.md")
        );
    }
}

#[test]
fn paste_omits_duration_without_a_clip_and_drops_empty_quotes() {
    let markdown = Path::new("/tmp/demo/take.md");
    let stills = paste(Some("Narration."), 2, 0, 99_000, markdown);
    assert_eq!(stills, "\"Narration.\"\n\n2 screenshots: /tmp/demo/take.md");
    assert!(!stills.contains('('));

    let silent = paste(None, 2, 1, 2_000, markdown);
    assert_eq!(
        silent,
        "No narration.\n\n2 screenshots, 1 clip (0:02): /tmp/demo/take.md"
    );
}

#[test]
fn summary_reads_a_legacy_clip_folder() {
    let root = std::env::temp_dir().join(format!(
        "see-computer-legacy-summary-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let mov = root.join("legacy.mov");
    let dir = mov.with_extension("");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&mov, b"not a movie").unwrap();
    std::fs::write(dir.join("clip.md"), "# Legacy recording").unwrap();
    std::fs::write(
        dir.join("transcript.json"),
        r#"{"durationMs": 25000, "fullText": "Keep this recent."}"#,
    )
    .unwrap();

    let summary = summary(&mov).expect("legacy clip should remain in recents");
    assert!(mov.is_file(), "legacy movie stays outside its folder");
    assert_eq!(summary.markdown, dir.join("clip.md"));
    assert_eq!(
        summary.paste(),
        format!(
            "\"Keep this recent.\"\n\n1 clip (0:25): {}",
            dir.join("clip.md").display()
        )
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn summary_prefers_take_and_rebuilds_its_capture_tail() {
    let root = std::env::temp_dir().join(format!(
        "see-computer-take-summary-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let mov = root.join("current.mov");
    let dir = mov.with_extension("");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&mov, b"not a movie").unwrap();
    std::fs::write(dir.join("take.md"), "# Current take").unwrap();
    std::fs::write(dir.join("clip.md"), "# Stale legacy take").unwrap();
    std::fs::write(
        dir.join("transcript.json"),
        r#"{
                "fullText": "Inspect this.",
                "captures": [
                    {"type": "shot"},
                    {"type": "shot"},
                    {"type": "clip", "durationMs": 2000, "shots": []}
                ]
            }"#,
    )
    .unwrap();

    let summary = summary(&mov).expect("take should appear in recents");
    assert_eq!(summary.markdown, dir.join("take.md"));
    assert_eq!(
        summary.paste(),
        format!(
            "\"Inspect this.\"\n\n2 screenshots, 1 clip (0:02): {}",
            dir.join("take.md").display()
        )
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn summary_reads_a_nested_take_directory() {
    let root = std::env::temp_dir().join(format!(
        "see-computer-nested-summary-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("2026-08-28-10-12-09");
    std::fs::create_dir_all(dir.join("clips/001")).unwrap();
    std::fs::write(dir.join("clips/001/clip.mov"), b"movie").unwrap();
    std::fs::write(dir.join("take.md"), "# Nested take").unwrap();
    std::fs::write(
        dir.join("transcript.json"),
        r#"{
                "fullText": "Nested and recent.",
                "captures": [
                    {"type": "clip", "durationMs": 2918, "shots": []}
                ]
            }"#,
    )
    .unwrap();

    let summary = summary(&dir).expect("nested take should appear in recents");
    assert_eq!(summary.markdown, dir.join("take.md"));
    assert_eq!(
        summary.paste(),
        format!(
            "\"Nested and recent.\"\n\n1 clip (0:03): {}",
            dir.join("take.md").display()
        )
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shot_markdown_pairs_each_image_with_its_sentence() {
    let dir = std::env::temp_dir().join(format!(
        "see-computer-shot-markdown-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let first_shot = dir.join("first.png");
    let second_shot = dir.join("second.png");
    std::fs::write(&first_shot, b"first").unwrap();
    std::fs::write(&second_shot, b"second").unwrap();
    let captures = [
        SessionCapture::Shot(Shot {
            at_ms: 1_000,
            path: first_shot,
        }),
        SessionCapture::Shot(Shot {
            at_ms: 5_000,
            path: second_shot,
        }),
    ];
    let transcription = Transcription {
        text: Text::parse("First thing. Second thing."),
        segments: vec![
            segment(0, 4_000, "First thing."),
            segment(4_200, 9_000, "Second thing."),
        ],
    };
    package_session_with(&dir, 10_000, &transcription, &captures, |_, _, _, _| false).unwrap();
    let md = std::fs::read_to_string(dir.join("take.md")).unwrap();
    let first = md.find("First thing.").unwrap();
    let shot_one = md.find("shots/001.png").unwrap();
    let second = md.find("Second thing.").unwrap();
    let shot_two = md.find("shots/002.png").unwrap();
    assert!(first < shot_one && shot_one < second && second < shot_two);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn mixed_session_keeps_one_plain_paste_and_interleaves_captures() {
    let dir = std::env::temp_dir().join(format!(
        "see-computer-mixed-clip-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shots")).unwrap();
    let shot = dir.join("shots/001.png");
    std::fs::write(&shot, b"screenshot").unwrap();
    let mov = dir.with_extension("mov");
    std::fs::write(&mov, b"not a movie").unwrap();
    let transcription = Transcription {
        text: Text::parse("First thing. Second thing."),
        segments: vec![
            segment(0, 4_000, "First thing."),
            segment(4_000, 8_000, "Second thing."),
        ],
    };
    let captures = [
        SessionCapture::Shot(Shot {
            at_ms: 1_000,
            path: shot,
        }),
        SessionCapture::Clip(SessionClip {
            start_ms: 5_000,
            end_ms: 7_000,
            recording_start_ms: 5_250,
            path: mov.clone(),
            shots_ms: Vec::new(),
        }),
    ];

    let packaged = package_session(&dir, 8_000, &transcription, &captures).unwrap();
    assert_eq!(
        packaged.paste,
        format!(
            "\"First thing. Second thing.\"\n\n1 screenshot, 1 clip (0:02): {}",
            dir.join("take.md").display()
        )
    );
    let markdown = std::fs::read_to_string(packaged.markdown).unwrap();
    let first = markdown.find("First thing.").unwrap();
    let shot = markdown.find("shots/001.png").unwrap();
    let second = markdown.find("Second thing.").unwrap();
    let clip = markdown.find("Video clip at 0:05").unwrap();
    assert!(first < shot && shot < second && second < clip);
    assert!(!mov.exists());
    assert!(dir.join("clips/001/clip.mov").is_file());

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn blip_shot_lands_in_shots_without_breaking_numbering() {
    let root = std::env::temp_dir().join(format!(
        "see-computer-blip-shot-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("take");
    std::fs::create_dir_all(dir.join("shots")).unwrap();
    let first = dir.join("shots/001.png");
    let third = dir.join("shots/003.png");
    std::fs::write(&first, b"first screenshot").unwrap();
    std::fs::write(&third, b"third screenshot").unwrap();
    let mov = root.join("recording.mov");
    std::fs::write(&mov, b"movie").unwrap();
    let captures = [
        SessionCapture::Shot(Shot {
            at_ms: 500,
            path: first,
        }),
        SessionCapture::Clip(SessionClip {
            start_ms: 1_000,
            end_ms: 3_000,
            recording_start_ms: 1_250,
            path: mov,
            shots_ms: vec![1_800],
        }),
        SessionCapture::Shot(Shot {
            at_ms: 3_500,
            path: third,
        }),
    ];

    let packaged = package_session_with(
        &dir,
        4_000,
        &Transcription::empty(),
        &captures,
        |_mov, _at_ms, _tolerance_ms, out| std::fs::write(out, b"jpeg frame").is_ok(),
    )
    .unwrap();

    let mut names = std::fs::read_dir(dir.join("shots"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["001.png", "002.jpg", "003.png"]);
    assert_eq!(
        packaged.paste,
        format!(
            "No narration.\n\n3 screenshots, 1 clip (0:02): {}",
            dir.join("take.md").display()
        )
    );
    let transcript = std::fs::read_to_string(dir.join("transcript.json")).unwrap();
    assert!(transcript.contains(r#""file": "shots/002.jpg""#));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clips_are_nested_in_recording_order() {
    let root = std::env::temp_dir().join(format!(
        "see-computer-ordered-clips-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("take");
    std::fs::create_dir_all(&dir).unwrap();
    let first = root.join("later-stamp.mov");
    let second = root.join("latest-stamp.mov");
    std::fs::write(&first, b"first movie").unwrap();
    std::fs::write(&second, b"second movie").unwrap();
    let captures = [
        SessionCapture::Clip(SessionClip {
            start_ms: 1_000,
            end_ms: 2_000,
            recording_start_ms: 1_250,
            path: first,
            shots_ms: Vec::new(),
        }),
        SessionCapture::Clip(SessionClip {
            start_ms: 3_000,
            end_ms: 4_000,
            recording_start_ms: 3_250,
            path: second,
            shots_ms: Vec::new(),
        }),
    ];

    package_session_with(
        &dir,
        4_000,
        &Transcription::empty(),
        &captures,
        |_mov, _at_ms, _tolerance_ms, _out| false,
    )
    .unwrap();

    assert_eq!(
        std::fs::read(dir.join("clips/001/clip.mov")).unwrap(),
        b"first movie"
    );
    assert_eq!(
        std::fs::read(dir.join("clips/002/clip.mov")).unwrap(),
        b"second movie"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn one_clip_without_shots_uses_the_nested_layout() {
    let root = std::env::temp_dir().join(format!(
        "see-computer-flat-session-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mov = root.join("capture.mov");
    std::fs::write(&mov, b"not a movie").unwrap();
    let transcription = Transcription {
        text: Text::parse("Keep the common layout."),
        segments: vec![segment(0, 1_000, "Keep the common layout.")],
    };

    let packaged = package_single_clip(
        1_000,
        &transcription,
        SessionClip {
            start_ms: 0,
            end_ms: 1_000,
            recording_start_ms: 250,
            path: mov.clone(),
            shots_ms: Vec::new(),
        },
    )
    .unwrap();

    assert_eq!(packaged.markdown, root.join("capture/take.md"));
    assert!(!mov.exists());
    assert!(root.join("capture/clips/001/clip.mov").is_file());
    assert!(root.join("capture/clips/001/frames").is_dir());
    assert!(root.join("capture/transcript.json").is_file());
    assert!(!root.join("capture/clip.md").exists());
    assert!(!root.join("capture/session.md").exists());
    assert!(!root.join("capture/shots").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn durations_round_while_positions_floor() {
    assert_eq!(duration_timestamp(2_918), "0:03");
    assert_eq!(position_timestamp(2_918), "0:02");
    assert_eq!(duration_timestamp(65_500), "1:06");
    assert_eq!(position_timestamp(65_999), "1:05");
    assert_eq!(position_timestamp(3_725_000), "1:02:05");
}
