//! Opt-in, disk-backed uploads. Capture and transcription never wait for HTTP.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HOST: &str = "https://see-computer-shares.cocco.workers.dev";
const MAX_OBJECT: u64 = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct Sharing(Arc<Shared>);

struct Shared {
    root: PathBuf,
    host: String,
    state: Mutex<State>,
    wake: Condvar,
}

struct State {
    enabled: bool,
    jobs: BTreeMap<String, Job>,
    message: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Job {
    id: String,
    dir: PathBuf,
    created: bool,
    finalized: bool,
    cancelled: bool,
    ready: bool,
    error: Option<String>,
    sent: BTreeMap<String, String>,
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    retry_at: u64,
    #[serde(default)]
    created_at: u64,
}

#[derive(Serialize, Deserialize)]
struct Link {
    url: String,
    state: String,
    #[serde(default)]
    expires_at: u64,
}

struct Asset {
    name: String,
    source: Source,
    stamp: String,
}

enum Source {
    File(PathBuf),
    Bytes(Vec<u8>),
}

impl Sharing {
    pub fn start(enabled: bool) -> Self {
        let service = Self::load(crate::paths::app_support().join("sharing"), HOST, enabled);
        let worker = service.clone();
        crate::qos::spawn("see-uploads", crate::qos::Class::Upkeep, move || {
            worker.run()
        });
        service
    }

    fn load(root: PathBuf, host: &str, enabled: bool) -> Self {
        let mut jobs = BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(entry.path()) else {
                    continue;
                };
                let Ok(mut job) = serde_json::from_slice::<Job>(&bytes) else {
                    continue;
                };
                // A crash can happen between packaging and marking the queue finished.
                if !job.finalized && !job.cancelled {
                    if job.dir.join("take.md").is_file()
                        && job.dir.join("transcript.json").is_file()
                    {
                        job.finalized = true;
                    } else {
                        job.cancelled = true;
                    }
                }
                jobs.insert(job.id.clone(), job);
            }
        }
        Self(Arc::new(Shared {
            root,
            host: host.to_owned(),
            state: Mutex::new(State {
                enabled,
                jobs,
                message: String::new(),
            }),
            wake: Condvar::new(),
        }))
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self::load(PathBuf::new(), HOST, false)
    }

    pub fn enabled(&self) -> bool {
        self.0.state.lock().unwrap().enabled
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.0.state.lock().unwrap().enabled = enabled;
        self.0.wake.notify_one();
    }

    pub fn status(&self) -> String {
        let state = self.0.state.lock().unwrap();
        if !state.message.is_empty() {
            return state.message.clone();
        }
        let pending = state
            .jobs
            .values()
            .filter(|j| !j.ready && !j.cancelled && j.error.is_none())
            .count();
        if pending > 0 {
            format!(
                "{pending} upload{} {}",
                if pending == 1 { "" } else { "s" },
                if state.enabled { "queued" } else { "paused" }
            )
        } else {
            "10 shares/month · Kept for 30 days".to_owned()
        }
    }

    /// Called only for a capture, never for plain dictation. Generates the URL locally.
    pub fn begin(&self, dir: &Path) {
        let mut state = self.0.state.lock().unwrap();
        if !state.enabled || state.jobs.values().any(|job| job.dir == dir) {
            return;
        }
        let result = (|| -> io::Result<Job> {
            let id = random_hex(16)?;
            let job = Job {
                id,
                dir: dir.to_owned(),
                created: false,
                finalized: false,
                cancelled: false,
                ready: false,
                error: None,
                sent: BTreeMap::new(),
                attempts: 0,
                retry_at: 0,
                created_at: epoch(),
            };
            self.save(&job)?;
            self.write_link(&job)?;
            Ok(job)
        })();
        match result {
            Ok(job) => {
                state.jobs.insert(job.id.clone(), job);
            }
            Err(error) => state.message = format!("Could not queue share: {error}"),
        }
        self.0.wake.notify_one();
    }

    pub fn finalize(&self, dir: &Path) {
        self.change_dir(dir, |job| job.finalized = true);
    }

    pub fn cancel(&self, dir: Option<&Path>) {
        if let Some(dir) = dir {
            self.change_dir(dir, |job| {
                job.cancelled = true;
                job.retry_at = 0;
            });
        }
    }

    pub fn retry(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.message.clear();
        for job in state
            .jobs
            .values_mut()
            .filter(|job| !job.ready && !job.cancelled)
        {
            job.error = None;
            job.retry_at = 0;
            job.attempts = 0;
            let _ = self.save(job);
            let _ = self.write_link(job);
        }
        self.0.wake.notify_one();
    }

    pub fn revoke_latest(&self) {
        let mut state = self.0.state.lock().unwrap();
        if let Some(job) = state
            .jobs
            .values_mut()
            .filter(|j| !j.cancelled)
            .max_by(|a, b| a.dir.cmp(&b.dir))
        {
            job.cancelled = true;
            job.retry_at = 0;
            let _ = self.save(job);
            let _ = self.write_link(job);
            state.message = "Removing shared link…".to_owned();
        }
        self.0.wake.notify_one();
    }

    fn change_dir(&self, dir: &Path, change: impl FnOnce(&mut Job)) {
        let mut state = self.0.state.lock().unwrap();
        if let Some(job) = state.jobs.values_mut().find(|job| job.dir == dir) {
            change(job);
            let _ = self.save(job);
            let _ = self.write_link(job);
        }
        self.0.wake.notify_one();
    }

    fn save(&self, job: &Job) -> io::Result<()> {
        write_private(
            &self.0.root.join(format!("{}.json", job.id)),
            &serde_json::to_vec(job)?,
        )
    }

    fn write_link(&self, job: &Job) -> io::Result<()> {
        if !job.dir.is_dir() {
            return Ok(());
        }
        let state = if job.cancelled {
            "removed"
        } else if job.error.is_some() {
            "failed"
        } else if job.ready {
            "ready"
        } else {
            "uploading"
        };
        write_private(
            &job.dir.join("share.json"),
            &serde_json::to_vec(&Link {
                url: format!("{}/s/{}/", self.0.host, job.id),
                state: state.to_owned(),
                expires_at: job.created_at + 30 * 24 * 60 * 60,
            })?,
        )
    }

    fn run(&self) {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(4)))
            .http_status_as_error(false)
            .build()
            .into();
        loop {
            let jobs: Vec<Job> = {
                let state = self.0.state.lock().unwrap();
                state
                    .jobs
                    .values()
                    .filter(|job| {
                        job.retry_at <= epoch()
                            && (job.cancelled
                                || (state.enabled && !job.ready && job.error.is_none()))
                    })
                    .cloned()
                    .collect()
            };
            let mut progress = false;
            for mut job in jobs {
                if !job.dir.is_dir() {
                    job.cancelled = true;
                }
                match self.tick(&agent, &job) {
                    Ok(worked) => progress |= worked,
                    Err(error) => {
                        let mut state = self.0.state.lock().unwrap();
                        state.message = error.message.clone();
                        if let Some(current) = state.jobs.get_mut(&job.id) {
                            current.attempts = current.attempts.saturating_add(1);
                            current.retry_at = epoch() + (1_u64 << current.attempts.min(5));
                            if error.permanent && !current.cancelled {
                                current.error = Some(error.message);
                            }
                            let _ = self.save(current);
                            let _ = self.write_link(current);
                        }
                    }
                }
            }
            if !progress {
                let state = self.0.state.lock().unwrap();
                let _ = self.0.wake.wait_timeout(state, Duration::from_millis(200));
            }
        }
    }

    fn allowed(&self, id: &str) -> bool {
        let state = self.0.state.lock().unwrap();
        state.enabled
            && state
                .jobs
                .get(id)
                .is_some_and(|j| !j.cancelled && j.error.is_none())
    }

    fn tick(&self, agent: &ureq::Agent, job: &Job) -> Result<bool, UploadError> {
        if job.cancelled {
            // DELETE is retried even while sharing is off. It never uploads content.
            // Always send it: a create response may have been lost before local persistence.
            self.request(agent, "DELETE", job, "", &[])?;
            let mut state = self.0.state.lock().unwrap();
            state.jobs.remove(&job.id);
            state.message.clear();
            let _ = std::fs::remove_file(self.0.root.join(format!("{}.json", job.id)));
            let _ = std::fs::remove_dir_all(self.0.root.join(&job.id));
            return Ok(true);
        }
        if !self.allowed(&job.id) {
            return Ok(false);
        }
        if !job.created {
            self.request(agent, "POST", job, "", &[])?;
            self.update(&job.id, |j| j.created = true)?;
            return Ok(true);
        }
        let assets = self.assets(job)?;
        for asset in &assets {
            if job.sent.get(&asset.name) == Some(&asset.stamp) {
                continue;
            }
            if !self.allowed(&job.id) {
                return Ok(false);
            }
            let bytes = match &asset.source {
                Source::Bytes(bytes) => bytes.clone(),
                Source::File(path) => read_bounded(path)?,
            };
            // A playlist can only refer to segments that have reached R2.
            if asset.name.ends_with(".m3u8") && !playlist_uploaded(&asset.name, &bytes, &job.sent) {
                continue;
            }
            self.request(agent, "PUT", job, &asset.name, &bytes)?;
            self.update(&job.id, |j| {
                j.sent.insert(asset.name.clone(), asset.stamp.clone());
            })?;
            return Ok(true);
        }
        if job.finalized
            && assets
                .iter()
                .all(|a| job.sent.get(&a.name) == Some(&a.stamp))
        {
            if !manifest_uploaded(&assets, &job.sent) {
                return Err(UploadError::permanent(
                    "A recording file is missing. The share is incomplete.",
                ));
            }
            if !self.allowed(&job.id) {
                return Ok(false);
            }
            self.request(agent, "POST", job, "complete", &[])?;
            self.update(&job.id, |j| j.ready = true)?;
            let _ = std::fs::remove_dir_all(self.0.root.join(&job.id));
            return Ok(true);
        }
        Ok(false)
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut Job)) -> io::Result<()> {
        let mut state = self.0.state.lock().unwrap();
        state.message.clear();
        if let Some(job) = state.jobs.get_mut(id) {
            change(job);
            job.attempts = 0;
            job.retry_at = 0;
            self.save(job)?;
            self.write_link(job)?;
        }
        Ok(())
    }

    fn request(
        &self,
        agent: &ureq::Agent,
        method: &str,
        job: &Job,
        path: &str,
        bytes: &[u8],
    ) -> Result<(), UploadError> {
        let token = self.credential()?;
        let url = format!(
            "{}/api/shares/{}{}{}",
            self.0.host,
            job.id,
            if path.is_empty() { "" } else { "/" },
            path
        );
        let auth = format!("Bearer {token}");
        let response = match method {
            "POST" => agent.post(&url).header("Authorization", &auth).send(bytes),
            "DELETE" => agent.delete(&url).header("Authorization", &auth).call(),
            _ => agent.put(&url).header("Authorization", &auth).send(bytes),
        }
        .map_err(|_| UploadError::temporary("Upload paused. Retrying when connected."))?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) || (method == "DELETE" && status == 404) {
            return Ok(());
        }
        Err(match status {
            401 | 403 => {
                UploadError::permanent("Sharing credential was rejected. Local recording saved.")
            }
            413 => {
                UploadError::permanent("Share exceeds the beta size limit. Local recording saved.")
            }
            429 => UploadError::permanent(
                "Sharing allowance reached. Local recording saved. Retry later.",
            ),
            400 | 404 | 409 | 410 | 415 => {
                UploadError::permanent("Could not publish this share. Local recording saved.")
            }
            _ => UploadError::temporary("Upload paused. Retrying when connected."),
        })
    }

    fn credential(&self) -> io::Result<String> {
        let path = self.0.root.join("installation.key");
        match std::fs::read_to_string(&path) {
            Ok(key) if key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()) => Ok(key),
            Ok(_) => Err(io::Error::other("invalid sharing credential")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let key = random_hex(32)?;
                write_private(&path, key.as_bytes())?;
                Ok(key)
            }
            Err(error) => Err(error),
        }
    }

    fn assets(&self, job: &Job) -> Result<Vec<Asset>, UploadError> {
        let mut files = Vec::new();
        collect_files(&job.dir.join("live"), &mut files)?;
        collect_files(&job.dir.join("shots"), &mut files)?;
        if job.finalized {
            collect_files(&job.dir.join("clips"), &mut files)?;
        }
        files.sort();
        let mut assets = Vec::new();
        let mut playlists = Vec::new();
        let mut shots = Vec::new();
        for path in files {
            let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if !matches!(extension, "mp4" | "m4s" | "m3u8" | "png" | "jpg") {
                continue;
            }
            let name = path
                .strip_prefix(&job.dir)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .into_owned();
            if extension == "png" && !complete_png(&path) {
                continue;
            }
            if extension == "m3u8" && job.sent.contains_key(&name) {
                playlists.push(serde_json::json!({"playlist": name}));
            }
            let asset = if extension == "png" {
                let name = name.strip_suffix(".png").unwrap().to_owned() + ".jpg";
                let stamp = file_stamp(&path)?;
                let cache = self.0.root.join(&job.id).join(format!("{stamp}.jpg"));
                if job.sent.get(&name) != Some(&stamp) && !cache.is_file() {
                    std::fs::create_dir_all(cache.parent().unwrap())?;
                    let status = std::process::Command::new("/usr/bin/sips")
                        .args([
                            "-Z",
                            "2560",
                            "-s",
                            "format",
                            "jpeg",
                            "-s",
                            "formatOptions",
                            "85",
                        ])
                        .arg(&path)
                        .arg("--out")
                        .arg(&cache)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()?;
                    if !status.success() {
                        return Err(UploadError::temporary(
                            "Could not compress screenshot. Retrying.",
                        ));
                    }
                }
                Asset {
                    name,
                    source: Source::File(cache),
                    stamp,
                }
            } else {
                Asset {
                    name,
                    stamp: file_stamp(&path)?,
                    source: Source::File(path),
                }
            };
            if asset.name.starts_with("shots/") && job.sent.contains_key(&asset.name) {
                shots.push(serde_json::json!({"file": asset.name}));
            }
            assets.push(asset);
        }
        // Immutable init/media objects precede manifests that reference them.
        assets.sort_by_key(|a| {
            (
                a.name.ends_with(".m3u8"),
                !a.name.ends_with("init.mp4"),
                a.name.clone(),
            )
        });
        assets.push(bytes_asset(
            "live.json",
            serde_json::to_vec(&serde_json::json!({"clips": playlists, "shots": shots}))?,
        ));
        if job.finalized {
            let mut transcript: serde_json::Value =
                serde_json::from_slice(&read_bounded(&job.dir.join("transcript.json"))?)?;
            let mut markdown = String::from_utf8(read_bounded(&job.dir.join("take.md"))?)
                .map_err(io::Error::other)?;
            if let Some(captures) = transcript
                .get_mut("captures")
                .and_then(serde_json::Value::as_array_mut)
            {
                for capture in captures {
                    if let Some(playlist) = capture
                        .get("playlist")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                    {
                        if let Some(video) =
                            capture.get("video").and_then(serde_json::Value::as_str)
                        {
                            markdown = markdown.replace(video, &playlist);
                        }
                        capture["video"] = playlist.into();
                    }
                    if let Some(file) = capture
                        .get("file")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                    {
                        if let Some(base) = file.strip_suffix(".png") {
                            let jpeg = format!("{base}.jpg");
                            markdown = markdown.replace(&file, &jpeg);
                            capture["file"] = jpeg.into();
                        }
                    }
                }
            }
            assets.push(bytes_asset("take.md", markdown.into_bytes()));
            assets.push(bytes_asset(
                "transcript.json",
                serde_json::to_vec(&transcript)?,
            ));
        }
        Ok(assets)
    }
}

/// The original local path remains usable if sharing fails or a link is revoked.
pub fn paste_link(markdown: &Path, local_paste: String) -> String {
    let Some(dir) = markdown.parent() else {
        return local_paste;
    };
    let Some(link) = std::fs::read(dir.join("share.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Link>(&b).ok())
    else {
        return local_paste;
    };
    if !matches!(link.state.as_str(), "ready" | "uploading") {
        return local_paste;
    }
    if link.expires_at != 0 && link.expires_at <= epoch() {
        return local_paste;
    }
    // Only links belonging to our host may replace a local path.
    if !link.url.starts_with(&format!("{HOST}/s/")) {
        return local_paste;
    }
    let paste = local_paste.replace(&markdown.to_string_lossy().to_string(), &link.url);
    if link.state == "uploading" {
        format!("{paste}\nUpload queued. Local copy: {}", markdown.display())
    } else {
        paste
    }
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(&entry.path(), files)?;
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn playlist_uploaded(name: &str, bytes: &[u8], sent: &BTreeMap<String, String>) -> bool {
    if !bytes.windows(8).any(|part| part == b"#EXTINF:") {
        return false;
    }
    let Some((parent, _)) = name.rsplit_once('/') else {
        return false;
    };
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| {
            if let Some(map) = line.strip_prefix("#EXT-X-MAP:URI=\"") {
                map.split('"').next()
            } else if !line.starts_with('#') && !line.is_empty() {
                Some(line)
            } else {
                None
            }
        })
        .all(|file| sent.contains_key(&format!("{parent}/{file}")))
}

fn manifest_uploaded(assets: &[Asset], sent: &BTreeMap<String, String>) -> bool {
    let Some(asset) = assets.iter().find(|a| a.name == "transcript.json") else {
        return false;
    };
    let Source::Bytes(bytes) = &asset.source else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let Some(captures) = manifest
        .get("captures")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    captures.iter().all(|capture| {
        let media = capture
            .get("file")
            .or_else(|| capture.get("video"))
            .and_then(serde_json::Value::as_str);
        media.is_some_and(|name| sent.contains_key(name))
            && capture
                .get("frames")
                .and_then(serde_json::Value::as_array)
                .is_none_or(|frames| {
                    frames.iter().all(|frame| {
                        frame
                            .get("file")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|name| sent.contains_key(name))
                    })
                })
    })
}

fn bytes_asset(name: &str, bytes: Vec<u8>) -> Asset {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    Asset {
        name: name.to_owned(),
        stamp: format!("{:x}", hash.finish()),
        source: Source::Bytes(bytes),
    }
}

fn file_stamp(path: &Path) -> io::Result<String> {
    let meta = path.metadata()?;
    Ok(format!(
        "{}-{}",
        meta.len(),
        meta.modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_OBJECT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds 8 MiB upload limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_OBJECT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_OBJECT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds 8 MiB upload limit",
        ));
    }
    Ok(bytes)
}

fn complete_png(path: &Path) -> bool {
    use std::io::{Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut end = [0_u8; 12];
    file.seek(SeekFrom::End(-12)).is_ok()
        && file.read_exact(&mut end).is_ok()
        && end == [0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130]
}

fn random_hex(bytes: usize) -> io::Result<String> {
    let mut random = vec![0_u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| io::Error::other("missing parent"))?,
    )?;
    let part = path.with_extension("part");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&part)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(part, path)
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug)]
struct UploadError {
    message: String,
    permanent: bool,
}
impl UploadError {
    fn temporary(message: &str) -> Self {
        Self {
            message: message.to_owned(),
            permanent: false,
        }
    }
    fn permanent(message: &str) -> Self {
        Self {
            message: message.to_owned(),
            permanent: true,
        }
    }
}
impl From<io::Error> for UploadError {
    fn from(error: io::Error) -> Self {
        Self {
            message: format!("Upload paused: {error}"),
            permanent: error.kind() == io::ErrorKind::InvalidData,
        }
    }
}
impl From<serde_json::Error> for UploadError {
    fn from(error: serde_json::Error) -> Self {
        Self::permanent(&format!("Could not read recording: {error}"))
    }
}

#[cfg(test)]
mod tests;
