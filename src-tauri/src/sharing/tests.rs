use super::*;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};

fn setup(enabled: bool) -> (Sharing, PathBuf) {
    setup_at(HOST, enabled)
}

fn setup_at(host: &str, enabled: bool) -> (Sharing, PathBuf) {
    let root = std::env::temp_dir().join(format!("see-sharing-{}", random_hex(12).unwrap()));
    let dir = root.join("recording");
    std::fs::create_dir_all(&dir).unwrap();
    (Sharing::load(root.join("queue"), host, enabled), dir)
}

fn job(service: &Sharing) -> Job {
    service
        .0
        .state
        .lock()
        .unwrap()
        .jobs
        .values()
        .next()
        .unwrap()
        .clone()
}

fn link(dir: &Path) -> Link {
    serde_json::from_slice(&std::fs::read(dir.join("share.json")).unwrap()).unwrap()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .into()
}

/// Each request as its method and its path inside the share, with its body.
type Received = Vec<(String, Vec<u8>)>;

/// An actual TCP peer catches mistakes in HTTP status handling and send bodies.
struct Server {
    host: String,
    requests: Arc<Mutex<Received>>,
}

impl Server {
    fn requests(&self) -> Vec<String> {
        let requests = self.requests.lock().unwrap();
        requests.iter().map(|(line, _)| line.clone()).collect()
    }

    fn bodies(&self) -> BTreeMap<String, Vec<u8>> {
        self.requests.lock().unwrap().iter().cloned().collect()
    }
}

/// `reply` picks the status of each request. It runs before the answer goes
/// out, while the uploader waits for it.
fn server(reply: impl Fn(&str) -> u16 + Send + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let body = loop {
                let len = stream.read(&mut buffer).unwrap();
                bytes.extend_from_slice(&buffer[..len]);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .unwrap_or("0")
                        .trim()
                        .parse()
                        .unwrap();
                    if len == 0 || bytes.len() >= end + 4 + length {
                        break end + 4;
                    }
                }
            };
            let head = String::from_utf8_lossy(&bytes[..body]).into_owned();
            let mut words = head.split(' ');
            let (method, path) = (words.next().unwrap(), words.next().unwrap());
            // "/api/shares/" and the seven-character ID come before the path.
            let line = format!("{method} /{}", path[19..].trim_start_matches('/'));
            let status = reply(&line);
            seen.lock().unwrap().push((line, bytes[body..].to_vec()));
            write!(
                stream,
                "HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
    });
    Server { host, requests }
}

/// Gives every due job a turn, as the uploader's loop does, until none has work left.
fn run(service: &Sharing) {
    let client = agent();
    while service
        .due()
        .iter()
        .fold(false, |worked, job| service.step(&client, job) | worked)
    {}
}

/// One clip as the recorder leaves it: a playlist that lists `listed`
/// fragments, of which the first `written` are in the folder.
fn record(dir: &Path, listed: usize, written: usize) {
    let media = dir.join("live/clip.segments");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("init.mp4"), "init").unwrap();
    let mut playlist = String::from(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"init.mp4\"\n",
    );
    for index in 0..listed {
        if index < written {
            let fragment = format!("fragment {index}");
            std::fs::write(media.join(format!("segment-{index:03}.m4s")), fragment).unwrap();
        }
        playlist += &format!("#EXTINF:1.000000,\nsegment-{index:03}.m4s\n");
    }
    std::fs::write(media.join("index.m3u8"), playlist).unwrap();
}

/// The files packaging adds when the take ends.
fn package(dir: &Path) {
    let playlist = dir.join("live/clip.segments/index.m3u8");
    let ended = std::fs::read_to_string(&playlist).unwrap() + "#EXT-X-ENDLIST\n";
    std::fs::write(playlist, ended).unwrap();
    std::fs::write(
        dir.join("transcript.json"),
        r#"{"fullText":"Look here.","captures":[{"type":"clip","video":"clips/001/clip.mov","playlist":"live/clip.segments/index.m3u8","frames":[]}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("take.md"),
        "Look here.\n\n[Clip](clips/001/clip.mov)\n",
    )
    .unwrap();
}

fn clip(fragments: usize) -> Vec<String> {
    let mut requests = vec!["PUT /live/clip.segments/init.mp4".to_owned()];
    requests.extend((0..fragments).map(|i| format!("PUT /live/clip.segments/segment-{i:03}.m4s")));
    requests.push("PUT /live/clip.segments/index.m3u8".to_owned());
    requests
}

#[test]
fn finalized_take_reports_the_limiting_quota_before_paste() {
    for (parts, part_bytes, limit) in [
        (1496, 0, "1,500 files"),
        (32, 8 * 1024 * 1024, "256 MiB"),
        (1, 8 * 1024 * 1024 + 1, "8 MiB"),
    ] {
        let (service, dir) = setup(true);
        service.begin(&dir);
        record(&dir, parts, parts);
        package(&dir);
        if part_bytes > 0 {
            for index in 0..parts {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(dir.join(format!("live/clip.segments/segment-{index:03}.m4s")))
                    .unwrap()
                    .set_len(part_bytes)
                    .unwrap();
            }
        }
        service.finalize(&dir);
        let local = dir.join("take.md").display().to_string();
        assert_eq!(paste_link(&dir.join("take.md"), local.clone()), local);
        assert!(service.status().contains(limit), "{}", service.status());
        let restarted = Sharing::load(service.0.root.clone(), HOST, true);
        assert!(restarted.status().contains(limit));
        service.retry();
        assert_eq!(paste_link(&dir.join("take.md"), local.clone()), local);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}

#[test]
fn a_legacy_upload_with_unknown_sizes_waits_for_confirmation_only_if_it_might_exceed_quota() {
    let (service, dir) = setup(true);
    service.begin(&dir);
    record(&dir, 1, 1);
    package(&dir);
    let id = job(&service).id;
    service
        .update(&id, |j| {
            for index in 0..32 {
                j.sent
                    .insert(format!("live/previous-{index}.m4s"), "old-stamp".into());
            }
        })
        .unwrap();
    service.finalize(&dir);
    assert_eq!(link(&dir).state, "checking");
    service
        .update(&id, |j| {
            j.uploaded_bytes.insert("live/previous-0.m4s".into(), 100);
        })
        .unwrap();
    service.finalize(&dir);
    assert_eq!(link(&dir).state, "uploading");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_take_at_the_file_limit_can_share_while_uploading() {
    let (service, dir) = setup(true);
    service.begin(&dir);
    record(&dir, 1495, 1495);
    package(&dir);
    assert!(service.finalize(&dir));
    assert_eq!(link(&dir).state, "uploading");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_failed_checkpoint_keeps_an_old_link_out_of_the_initial_paste() {
    let (service, dir) = setup(true);
    service.begin(&dir);
    record(&dir, 1, 1);
    package(&dir);
    let queue_file = service.0.root.join(format!("{}.json", job(&service).id));
    std::fs::remove_file(&queue_file).unwrap();
    std::fs::create_dir(&queue_file).unwrap();
    assert!(!service.finalize(&dir));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn off_never_creates_a_job_or_credential_and_pauses_existing_jobs() {
    let (service, dir) = setup(false);
    service.begin(&dir);
    assert!(service.0.state.lock().unwrap().jobs.is_empty());
    assert!(!service.0.root.exists());
    service.set_enabled(true);
    service.begin(&dir);
    service.set_enabled(false);
    assert!(!service.tick(&agent(), &job(&service)).unwrap());
    assert!(!service.0.root.join("installation.key").exists());
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn reload_resumes_saved_takes_and_revokes_interrupted_recordings() {
    let (service, dir) = setup(true);
    service.begin(&dir);
    let loaded = Sharing::load(service.0.root.clone(), HOST, true);
    assert!(job(&loaded).cancelled);
    std::fs::write(dir.join("take.md"), "saved").unwrap();
    std::fs::write(dir.join("transcript.json"), "{}").unwrap();
    let loaded = Sharing::load(service.0.root.clone(), HOST, true);
    assert!(job(&loaded).finalized);
    assert!(!job(&loaded).cancelled);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn failed_put_keeps_chunk_pending_and_successful_retry_commits_it() {
    let failing = AtomicBool::new(true);
    let server = server(move |_| match failing.swap(false, Ordering::SeqCst) {
        true => 503,
        false => 201,
    });
    let (service, dir) = setup_at(&server.host, true);
    service.begin(&dir);
    let id = job(&service).id;
    service.update(&id, |j| j.created = true).unwrap();
    let segments = dir.join("live/one.segments");
    std::fs::create_dir_all(&segments).unwrap();
    std::fs::write(segments.join("init.mp4"), "init fixture").unwrap();
    let error = service.tick(&agent(), &job(&service)).unwrap_err();
    assert!(!error.permanent);
    assert!(job(&service).sent.is_empty());
    assert_eq!(server.requests(), ["PUT /live/one.segments/init.mp4"]);
    assert_eq!(
        server.bodies()["PUT /live/one.segments/init.mp4"],
        b"init fixture"
    );
    assert!(
        !job(&service).finalized,
        "chunk is sent while recording is still live"
    );
    assert!(service.tick(&agent(), &job(&service)).unwrap());
    assert!(job(&service)
        .sent
        .contains_key("live/one.segments/init.mp4"));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn cancel_revokes_even_when_create_response_was_lost_and_sharing_is_paused() {
    let server = server(|_| 204);
    let (service, dir) = setup_at(&server.host, true);
    service.begin(&dir);
    service.cancel(Some(&dir));
    service.set_enabled(false);
    assert!(service.tick(&agent(), &job(&service)).unwrap());
    assert_eq!(server.requests(), ["DELETE /"]);
    assert!(service.0.state.lock().unwrap().jobs.is_empty());
    assert_eq!(link(&dir).state, "removed");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_finished_backlog_uploads_in_one_turn_and_publishes() {
    let server = server(|_| 201);
    let (service, dir) = setup_at(&server.host, true);
    service.begin(&dir);
    record(&dir, 12, 12);
    package(&dir);
    service.finalize(&dir);
    let client = agent();
    assert!(service.step(&client, &job(&service)));
    assert!(service.step(&client, &job(&service)));

    let mut expected = vec!["POST /".to_owned()];
    expected.extend(clip(12));
    expected.extend(["PUT /live.json", "PUT /take.md", "PUT /transcript.json"].map(str::to_owned));
    expected.push("POST /complete".to_owned());
    assert_eq!(server.requests(), expected);
    assert_eq!(link(&dir).state, "ready");
    let saved = Sharing::load(service.0.root.clone(), &server.host, true);
    assert!(job(&saved).ready);
    assert_eq!(job(&saved).sent.len(), 17);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_finished_take_missing_a_fragment_fails_at_once_and_publishes_when_it_returns() {
    let server = server(|_| 201);
    let (service, dir) = setup_at(&server.host, true);
    service.begin(&dir);
    record(&dir, 3, 2);
    package(&dir);
    service.finalize(&dir);
    assert_eq!(job(&service).error.as_deref(), Some(MISSING));
    assert_eq!(service.status(), MISSING);
    assert_eq!(link(&dir).state, "failed");
    let local = format!("Capture: {}", dir.join("take.md").display());
    assert_eq!(paste_link(&dir.join("take.md"), local.clone()), local);

    // A retry while the fragment is still gone fails again and uploads nothing.
    service.retry();
    run(&service);
    assert_eq!(server.requests(), ["POST /"]);
    assert_eq!(job(&service).error.as_deref(), Some(MISSING));
    assert_eq!(link(&dir).state, "failed");

    std::fs::write(dir.join("live/clip.segments/segment-002.m4s"), "fragment 2").unwrap();
    service.retry();
    run(&service);
    assert!(job(&service).ready);
    assert_eq!(server.requests().last().unwrap(), "POST /complete");
    assert_eq!(link(&dir).state, "ready");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_playlist_waits_without_failing_while_its_fragment_is_still_arriving() {
    let server = server(|_| 201);
    let (service, dir) = setup_at(&server.host, true);
    service.begin(&dir);
    record(&dir, 2, 1);
    run(&service);
    assert_eq!(
        server.requests()[1..],
        [
            "PUT /live/clip.segments/init.mp4",
            "PUT /live/clip.segments/segment-000.m4s",
            "PUT /live.json",
        ]
    );
    assert_eq!(
        server.bodies()["PUT /live.json"],
        br#"{"clips":[],"shots":[]}"#
    );
    assert!(job(&service).error.is_none());
    assert_eq!(link(&dir).state, "uploading");

    std::fs::write(dir.join("live/clip.segments/segment-001.m4s"), "fragment 1").unwrap();
    run(&service);
    assert_eq!(
        server.requests()[4..],
        [
            "PUT /live/clip.segments/segment-001.m4s",
            "PUT /live/clip.segments/index.m3u8",
            "PUT /live.json",
        ]
    );
    assert_eq!(
        server.bodies()["PUT /live.json"],
        br#"{"clips":[{"playlist":"live/clip.segments/index.m3u8"}],"shots":[]}"#
    );
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_crash_between_an_upload_and_its_saved_progress_sends_it_again_and_publishes() {
    let crashed = std::env::temp_dir().join(format!("see-crashed-{}", random_hex(12).unwrap()));
    let (queue, copy) = (Arc::new(Mutex::new(PathBuf::new())), crashed.clone());
    let live = queue.clone();
    // The queue folder as a crash would leave it while this upload is in flight.
    let server = server(move |request| {
        if request == "PUT /live/clip.segments/segment-001.m4s" && !copy.exists() {
            std::fs::create_dir_all(&copy).unwrap();
            for entry in std::fs::read_dir(&*live.lock().unwrap()).unwrap().flatten() {
                if entry.path().is_file() {
                    std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
                }
            }
        }
        201
    });
    let (service, dir) = setup_at(&server.host, true);
    queue.lock().unwrap().clone_from(&service.0.root);
    service.begin(&dir);
    record(&dir, 3, 3);
    package(&dir);
    service.finalize(&dir);
    run(&service);
    assert!(job(&service).ready);
    let (first, uploads) = (server.bodies(), server.requests().len());

    let resumed = Sharing::load(crashed.clone(), &server.host, true);
    assert!(!job(&resumed).ready);
    assert!(!job(&resumed)
        .sent
        .contains_key("live/clip.segments/segment-001.m4s"));
    run(&resumed);
    assert!(job(&resumed).ready);
    let again = server.requests().split_off(uploads);
    assert!(again.contains(&"PUT /live/clip.segments/segment-001.m4s".to_owned()));
    assert_eq!(again.last().unwrap(), "POST /complete");
    assert_eq!(server.bodies(), first);
    std::fs::remove_dir_all(crashed).unwrap();
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_cancel_stops_the_turn_and_a_cancel_during_a_retry_wait_is_due_at_once() {
    let target = Arc::new(Mutex::new(None::<(Sharing, PathBuf)>));
    let (user, refuse) = (target.clone(), Arc::new(AtomicBool::new(false)));
    let refusing = refuse.clone();
    let server = server(move |request| {
        if refusing.load(Ordering::SeqCst) && request.starts_with("PUT") {
            return 503;
        }
        if request.ends_with("segment-000.m4s") {
            let (service, dir) = user.lock().unwrap().clone().unwrap();
            service.cancel(Some(&dir));
        }
        204
    });
    let (service, dir) = setup_at(&server.host, true);
    *target.lock().unwrap() = Some((service.clone(), dir.clone()));
    service.begin(&dir);
    record(&dir, 3, 3);
    run(&service);
    let mut expected = vec!["POST /".to_owned()];
    expected.extend_from_slice(&clip(3)[..2]);
    expected.push("DELETE /".to_owned());
    assert_eq!(server.requests(), expected);
    assert_eq!(link(&dir).state, "removed");
    assert!(service.0.state.lock().unwrap().jobs.is_empty());

    let other = dir.with_file_name("second");
    record(&other, 1, 1);
    service.begin(&other);
    refuse.store(true, Ordering::SeqCst);
    run(&service);
    assert_eq!(
        server.requests()[4..],
        ["POST /", "PUT /live/clip.segments/init.mp4"]
    );
    assert!(service.due().is_empty(), "the job waits before it retries");
    service.cancel(Some(&other));
    run(&service);
    assert_eq!(server.requests().last().unwrap(), "DELETE /");
    assert!(service.0.state.lock().unwrap().jobs.is_empty());
    assert_eq!(link(&other).state, "removed");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn the_link_file_is_written_only_when_its_state_changes() {
    use std::os::unix::fs::MetadataExt;
    let server = server(|_| 201);
    let (service, dir) = setup_at(&server.host, true);
    service.begin(&dir);
    record(&dir, 3, 3);
    let written = || {
        let file = dir.join("share.json").metadata().unwrap();
        (file.ino(), file.modified().unwrap())
    };
    let begun = written();
    run(&service);
    assert_eq!(job(&service).sent.len(), 6);
    assert_eq!(written(), begun);

    package(&dir);
    service.finalize(&dir);
    run(&service);
    assert_eq!(link(&dir).state, "ready");
    assert_ne!(written(), begun);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn reading_the_state_does_not_wait_for_the_queue_file() {
    let (service, dir) = setup(true);
    service.begin(&dir);
    let id = job(&service).id;
    // A pipe that nobody reads holds the next write of the queue file open.
    let pipe = service.0.root.join(format!("{id}.part"));
    assert!(std::process::Command::new("mkfifo")
        .arg(&pipe)
        .status()
        .unwrap()
        .success());
    let (writer, saved) = (service.clone(), id.clone());
    let writing = std::thread::spawn(move || writer.update(&saved, |job| job.created = true));
    std::thread::sleep(Duration::from_millis(200));
    let (reader, answer) = (service.clone(), std::sync::mpsc::channel());
    std::thread::spawn(move || answer.0.send(reader.enabled()));
    let read = answer.1.recv_timeout(Duration::from_secs(2));
    let reading = std::fs::File::open(&pipe).unwrap();
    let _ = writing.join().unwrap();
    drop(reading);
    assert_eq!(read, Ok(true), "a state read waited for the queue file");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn share_ids_are_seven_base62_characters() {
    for _ in 0..50 {
        let id = random_id().unwrap();
        assert_eq!(id.len(), 7);
        assert!(id.bytes().all(|b| b.is_ascii_alphanumeric()), "{id}");
    }
}

#[test]
fn playlist_waits_for_init_and_every_referenced_chunk() {
    let name = "live/one/index.m3u8";
    let bytes = b"#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\nsegment-000.m4s\n".to_vec();
    let playlist = Playlist {
        parts: playlist_parts(name, &bytes),
        asset: bytes_asset(name, bytes),
    };
    let mut sent = BTreeMap::new();
    assert!(!playlist.lists_only(|part| sent.contains_key(part)));
    sent.insert("live/one/init.mp4".to_owned(), "one");
    assert!(!playlist.lists_only(|part| sent.contains_key(part)));
    sent.insert("live/one/segment-000.m4s".to_owned(), "two");
    assert!(playlist.lists_only(|part| sent.contains_key(part)));
    let empty = b"#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n";
    assert_eq!(playlist_parts(name, empty), None);
}

#[test]
fn missing_media_cannot_be_marked_complete() {
    let (_, dir) = setup(true);
    std::fs::write(
        dir.join("transcript.json"),
        r#"{"captures":[{"type":"shot","file":"shots/001.png"}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("take.md"), "[Shot](shots/001.png)").unwrap();
    let incomplete = |sent: &Sent| Folder::finished(&dir, sent).is_err();
    let mut sent = BTreeMap::new();
    assert!(incomplete(&sent));
    sent.insert("shots/001.jpg".into(), "hash".into());
    assert!(!incomplete(&sent));
    sent.clear();
    std::fs::create_dir_all(dir.join("shots")).unwrap();
    std::fs::write(
        dir.join("shots/001.png"),
        [0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130],
    )
    .unwrap();
    assert!(!incomplete(&sent));
    std::fs::write(
        dir.join("transcript.json"),
        r#"{"captures":[{"type":"shot"}]}"#,
    )
    .unwrap();
    assert!(incomplete(&sent));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn finalized_manifest_uses_remote_media_and_keeps_local_originals() {
    let (service, dir) = setup(true);
    service.begin(&dir);
    record(&dir, 1, 1);
    std::fs::create_dir_all(dir.join("shots")).unwrap();
    std::fs::write(
        dir.join("shots/001.png"),
        include_bytes!("../../icons/tray.png"),
    )
    .unwrap();
    let source = r#"{"captures":[{"type":"clip","video":"clips/001/clip.mov","playlist":"live/clip.segments/index.m3u8"},{"type":"shot","file":"shots/001.png"}],"fullText":"Swedish och English"}"#;
    std::fs::write(dir.join("transcript.json"), source).unwrap();
    std::fs::write(
        dir.join("take.md"),
        "[Clip](clips/001/clip.mov) [Shot](shots/001.png)",
    )
    .unwrap();
    service.finalize(&dir);
    let [markdown, transcript] = documents(&dir).unwrap().assets;
    let (Source::Bytes(markdown), Source::Bytes(transcript)) = (markdown.source, transcript.source)
    else {
        panic!()
    };
    assert_eq!(
        String::from_utf8(markdown).unwrap(),
        "[Clip](live/clip.segments/index.m3u8) [Shot](shots/001.jpg)"
    );
    let remote: serde_json::Value = serde_json::from_slice(&transcript).unwrap();
    assert_eq!(
        remote["captures"][0]["video"],
        "live/clip.segments/index.m3u8"
    );
    assert_eq!(remote["captures"][1]["file"], "shots/001.jpg");
    assert_eq!(
        std::fs::read_to_string(dir.join("transcript.json")).unwrap(),
        source
    );
    let local = format!("Capture: {}", dir.join("take.md").display());
    let link = format!("Capture: {HOST}/s/{}/", job(&service).id);
    // Still uploading: the paste is the link alone, without the local path.
    assert_eq!(paste_link(&dir.join("take.md"), local.clone()), link);
    service
        .update(&job(&service).id, |j| j.error = Some("quota".into()))
        .unwrap();
    assert_eq!(paste_link(&dir.join("take.md"), local.clone()), local);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn only_a_live_link_on_our_host_is_eligible() {
    let url = format!("{HOST}/s/AbCdEf1/");
    let file = |state: &str, expires_at: u64, url: &str| {
        format!(r#"{{"url":"{url}","state":"{state}","expires_at":{expires_at}}}"#)
    };
    let later = epoch() + 60;
    for state in ["ready", "uploading"] {
        assert_eq!(eligible_url(&file(state, later, &url)), Some(url.clone()));
    }
    for state in ["failed", "removed"] {
        assert_eq!(eligible_url(&file(state, later, &url)), None);
    }
    assert_eq!(eligible_url(&file("ready", epoch() - 1, &url)), None);
    assert_eq!(
        eligible_url(&file("ready", later, "https://example.com/s/AbCdEf1/")),
        None
    );
    assert_eq!(eligible_url("not a link file"), None);
    // The first beta wrote no expiry.
    assert_eq!(
        eligible_url(&format!(r#"{{"url":"{url}","state":"ready"}}"#)),
        Some(url)
    );
}

#[test]
fn credential_is_private_and_stable_across_restarts() {
    use std::os::unix::fs::PermissionsExt;
    let (service, dir) = setup(true);
    let key = service.credential().unwrap();
    assert_eq!(key.len(), 64);
    assert_eq!(
        Sharing::load(service.0.root.clone(), HOST, true)
            .credential()
            .unwrap(),
        key
    );
    assert_eq!(
        service
            .0
            .root
            .join("installation.key")
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

/// Explicit integration test. Uploads only the generated gray video and sine-wave audio.
#[test]
#[ignore = "requires deployed R2 beta and compiled scripts/test-recorder.m"]
fn live_r2_synthetic_recording() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned();
    let root = repo
        .join(".audit/r2-integration")
        .join(random_hex(8).unwrap());
    let dir = root.join("take");
    std::fs::create_dir_all(dir.join("live")).unwrap();
    let service = Sharing::load(root.join("queue"), HOST, true);
    service.begin(&dir);
    let started = std::time::Instant::now();
    let mut recorder =
        std::process::Command::new(repo.join(".audit/native-media-test/test-recorder"))
            .arg(dir.join("live"))
            .spawn()
            .unwrap();
    let client = agent();
    let mut first_upload = None;
    while recorder.try_wait().unwrap().is_none() {
        service.tick(&client, &job(&service)).unwrap();
        if first_upload.is_none() && job(&service).sent.keys().any(|key| key.ends_with(".m4s")) {
            first_upload = Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(recorder.wait().unwrap().success());
    assert!(
        first_upload.is_some(),
        "media must reach R2 before recording stops"
    );
    std::fs::write(
        dir.join("take.md"),
        "# Synthetic recording\n\nHej och hello.\n\n[Clip](clips/001/clip.mov)\n",
    )
    .unwrap();
    std::fs::write(dir.join("transcript.json"), r#"{"fullText":"Hej och hello. Synthetic video and audio, with local transcription preserved.","durationMs":4000,"captures":[{"type":"clip","video":"clips/001/clip.mov","playlist":"live/clip.segments/index.m3u8","durationMs":4000,"frames":[]}]}"#).unwrap();
    service.finalize(&dir);
    while !job(&service).ready {
        assert!(started.elapsed() < Duration::from_secs(45));
        service.tick(&client, &job(&service)).unwrap();
    }
    let current = job(&service);
    let url = format!("{HOST}/s/{}/", current.id);
    assert_eq!(
        client
            .get(&format!("{url}status.json"))
            .call()
            .unwrap()
            .status()
            .as_u16(),
        200
    );
    let response = client
        .get(&format!("{url}live/clip.segments/init.mp4"))
        .header("Range", "bytes=0-7")
        .call()
        .unwrap();
    assert_eq!(response.status().as_u16(), 206);
    println!("Synthetic share {url}");
    println!(
        "First media uploaded after {} ms; ready after {} ms",
        first_upload.unwrap().as_millis(),
        started.elapsed().as_millis()
    );
    println!("Local integration artifacts {}", root.display());
}
