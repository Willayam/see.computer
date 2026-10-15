use super::*;
use std::net::TcpListener;

fn setup(enabled: bool) -> (Sharing, PathBuf) {
    let root = std::env::temp_dir().join(format!("see-sharing-{}", random_hex(12).unwrap()));
    let dir = root.join("recording");
    std::fs::create_dir_all(&dir).unwrap();
    (Sharing::load(root.join("queue"), HOST, enabled), dir)
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

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .into()
}

// An actual TCP peer catches mistakes in HTTP status handling and send bodies.
fn peer(status: u16) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let len = stream.read(&mut buffer).unwrap();
            if len == 0 {
                break;
            }
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
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        write!(
            stream,
            "HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (host, thread)
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
    let (host, peer_thread) = peer(503);
    let (base, dir) = setup(true);
    let service = Sharing::load(base.0.root.clone(), &host, true);
    service.begin(&dir);
    let id = job(&service).id;
    service.update(&id, |j| j.created = true).unwrap();
    let segments = dir.join("live/one.segments");
    std::fs::create_dir_all(&segments).unwrap();
    std::fs::write(segments.join("init.mp4"), "init fixture").unwrap();
    let error = service.tick(&agent(), &job(&service)).unwrap_err();
    assert!(!error.permanent);
    assert!(job(&service).sent.is_empty());
    let request = peer_thread.join().unwrap();
    assert!(request.starts_with(&format!("PUT /api/shares/{id}/live/one.segments/init.mp4 ")));
    assert!(request.ends_with("init fixture"));
    assert!(
        !job(&service).finalized,
        "chunk is sent while recording is still live"
    );
    let (host, peer_thread) = peer(201);
    // Preserve the active job, since restart correctly cancels an unfinished take.
    let retry = Sharing::load(service.0.root.clone(), &host, true);
    retry
        .0
        .state
        .lock()
        .unwrap()
        .jobs
        .insert(id.clone(), job(&service));
    assert!(retry.tick(&agent(), &job(&retry)).unwrap());
    peer_thread.join().unwrap();
    assert!(job(&retry).sent.contains_key("live/one.segments/init.mp4"));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn cancel_revokes_even_when_create_response_was_lost_and_sharing_is_paused() {
    let (host, peer_thread) = peer(204);
    let (base, dir) = setup(true);
    let service = Sharing::load(base.0.root.clone(), &host, true);
    service.begin(&dir);
    service.cancel(Some(&dir));
    service.set_enabled(false);
    assert!(service.tick(&agent(), &job(&service)).unwrap());
    assert!(peer_thread
        .join()
        .unwrap()
        .starts_with("DELETE /api/shares/"));
    assert!(service.0.state.lock().unwrap().jobs.is_empty());
    let link: Link =
        serde_json::from_slice(&std::fs::read(dir.join("share.json")).unwrap()).unwrap();
    assert_eq!(link.state, "removed");
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
    let playlist = b"#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\nsegment-000.m4s\n";
    let mut sent = BTreeMap::new();
    assert!(!playlist_uploaded("live/one/index.m3u8", playlist, &sent));
    sent.insert("live/one/init.mp4".into(), "one".into());
    assert!(!playlist_uploaded("live/one/index.m3u8", playlist, &sent));
    sent.insert("live/one/segment-000.m4s".into(), "two".into());
    assert!(playlist_uploaded("live/one/index.m3u8", playlist, &sent));
}

#[test]
fn missing_media_cannot_be_marked_complete() {
    let assets = vec![bytes_asset(
        "transcript.json",
        br#"{"captures":[{"type":"shot","file":"shots/001.jpg"}]}"#.to_vec(),
    )];
    let mut sent = BTreeMap::new();
    sent.insert("transcript.json".into(), "hash".into());
    assert!(!manifest_uploaded(&assets, &sent));
    sent.insert("shots/001.jpg".into(), "hash".into());
    assert!(manifest_uploaded(&assets, &sent));
}

#[test]
fn finalized_manifest_uses_remote_media_and_keeps_local_originals() {
    let (service, dir) = setup(true);
    service.begin(&dir);
    let source = r#"{"captures":[{"type":"clip","video":"clips/001/clip.mov","playlist":"live/one.segments/index.m3u8"},{"type":"shot","file":"shots/001.png"}],"fullText":"Swedish och English"}"#;
    std::fs::write(dir.join("transcript.json"), source).unwrap();
    std::fs::write(
        dir.join("take.md"),
        "[Clip](clips/001/clip.mov) [Shot](shots/001.png)",
    )
    .unwrap();
    service.finalize(&dir);
    let assets = service.assets(&job(&service)).unwrap();
    assert!(!assets.iter().any(|a| a.name == "share.json"));
    let Source::Bytes(bytes) = &assets
        .iter()
        .find(|a| a.name == "transcript.json")
        .unwrap()
        .source
    else {
        panic!()
    };
    let remote: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(
        remote["captures"][0]["video"],
        "live/one.segments/index.m3u8"
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
