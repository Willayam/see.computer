//! Opt-in, disk-backed uploads. Capture and transcription never wait for HTTP.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HOST: &str = "https://see.computer";
const MAX_OBJECT: u64 = 8 * 1024 * 1024;
const MAX_SHARE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHARE_FILES: usize = 1500;
const FILE_LIMIT: &str = "Share exceeds 1,500 files. Local recording saved.";
const BYTE_LIMIT: &str = "Share exceeds 256 MiB. Local recording saved.";
const OBJECT_LIMIT: &str = "A recording file exceeds 8 MiB. Local recording saved.";
/// How long one job uploads before it saves its progress and another job gets a turn.
const TURN: Duration = Duration::from_secs(1);
const MISSING: &str = "A recording file is missing. The share is incomplete.";

/// Upload name to the stamp of the version the share holds.
type Sent = BTreeMap<String, String>;

#[derive(Clone)]
pub struct Sharing(Arc<Shared>);

struct Shared {
    root: PathBuf,
    host: String,
    state: Mutex<State>,
    wake: Condvar,
    /// Orders writes of the queue and link files, and remembers the link state
    /// last written for each job. Lock it before `state` and never while
    /// holding `state`, so reading or changing the state never waits on a file
    /// sync.
    disk: Mutex<BTreeMap<String, &'static str>>,
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
    sent: Sent,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    uploaded_bytes: BTreeMap<String, u64>,
    #[serde(default)]
    quota: Quota,
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    retry_at: u64,
    #[serde(default)]
    created_at: u64,
}

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Quota {
    #[default]
    Unchecked,
    WithinLimits,
    AwaitingServer,
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

/// What one walk of a take folder found to upload.
struct Folder {
    /// Upload name and local path of each media file, init segments first.
    media: Vec<(String, PathBuf)>,
    playlists: Vec<Playlist>,
}

struct Playlist {
    asset: Asset,
    /// The init and media files it lists. `None` until it lists any media.
    parts: Option<Vec<String>>,
}

/// The finished take's Markdown and transcript as the share serves them.
struct Documents {
    assets: [Asset; 2],
    /// The media the transcript refers to. `None` when a capture names no file.
    captured: Option<Vec<String>>,
}

struct Turn {
    started: Instant,
    worked: bool,
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
        let unchecked: Vec<_> = jobs
            .values()
            .filter(|job| {
                job.finalized
                    && !job.cancelled
                    && !job.ready
                    && job.error.is_none()
                    && job.quota == Quota::Unchecked
            })
            .map(|job| job.id.clone())
            .collect();
        let sharing = Self(Arc::new(Shared {
            root,
            host: host.to_owned(),
            state: Mutex::new(State {
                enabled,
                jobs,
                message: String::new(),
            }),
            wake: Condvar::new(),
            disk: Mutex::new(BTreeMap::new()),
        }));
        for id in unchecked {
            let _ = sharing.persist(&id);
        }
        sharing
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
        if let Some((_, error)) = state
            .jobs
            .values()
            .filter(|job| !job.cancelled && !job.ready)
            .filter_map(|job| job.error.as_ref().map(|error| (job.created_at, error)))
            .max_by_key(|(at, _)| *at)
        {
            return error.clone();
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
        let mut links = self.0.disk.lock().unwrap();
        {
            let state = self.0.state.lock().unwrap();
            if !state.enabled || state.jobs.values().any(|job| job.dir == dir) {
                return;
            }
        }
        let result = (|| -> io::Result<Job> {
            let job = Job {
                id: random_id()?,
                dir: dir.to_owned(),
                created: false,
                finalized: false,
                cancelled: false,
                ready: false,
                error: None,
                sent: BTreeMap::new(),
                uploaded_bytes: BTreeMap::new(),
                quota: Quota::Unchecked,
                attempts: 0,
                retry_at: 0,
                created_at: epoch(),
            };
            self.write(&mut links, &job)?;
            Ok(job)
        })();
        {
            let mut state = self.0.state.lock().unwrap();
            match result {
                Ok(job) => {
                    state.jobs.insert(job.id.clone(), job);
                }
                Err(error) => state.message = format!("Could not queue share: {error}"),
            }
        }
        self.0.wake.notify_one();
    }

    /// The initial paste may use a link only after the finished inventory fits
    /// and its state was saved. Otherwise the caller keeps the local path.
    pub fn finalize(&self, dir: &Path) -> bool {
        let job = {
            let state = self.0.state.lock().unwrap();
            state.jobs.values().find(|job| job.dir == dir).cloned()
        };
        let Some(job) = job else { return false };
        let checked = Folder::finished(dir, &job.sent)
            .and_then(|(folder, documents)| self.quota(&job, &folder, &documents));
        let (quota, failure) = match checked {
            Ok(quota) => (quota, None),
            Err(error) if error.permanent => (Quota::Unchecked, Some(error.message)),
            Err(_) => (Quota::Unchecked, None),
        };
        if let Some(message) = &failure {
            self.0.state.lock().unwrap().message.clone_from(message);
        }
        let mut eligible = false;
        let saved = self.change_dir(dir, |job| {
            job.finalized = true;
            job.quota = quota;
            job.error = job.error.take().or(failure);
            eligible = !job.cancelled && job.error.is_none() && job.quota == Quota::WithinLimits;
        });
        eligible && saved
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
        let retried: Vec<String> = {
            let mut state = self.0.state.lock().unwrap();
            state.message.clear();
            let waiting = state.jobs.values_mut().filter(|j| !j.ready && !j.cancelled);
            waiting
                .map(|job| {
                    if job.error.is_some() {
                        job.quota = Quota::Unchecked;
                    }
                    job.error = None;
                    job.retry_at = 0;
                    job.attempts = 0;
                    job.id.clone()
                })
                .collect()
        };
        for id in retried {
            let _ = self.persist(&id);
        }
        self.0.wake.notify_one();
    }

    pub fn revoke_latest(&self) {
        let revoked = {
            let mut state = self.0.state.lock().unwrap();
            let latest = state.jobs.values_mut().filter(|j| !j.cancelled);
            let id = latest.max_by(|a, b| a.dir.cmp(&b.dir)).map(|job| {
                job.cancelled = true;
                job.retry_at = 0;
                job.id.clone()
            });
            if id.is_some() {
                state.message = "Removing shared link…".to_owned();
            }
            id
        };
        if let Some(id) = revoked {
            let _ = self.persist(&id);
        }
        self.0.wake.notify_one();
    }

    fn change_dir(&self, dir: &Path, change: impl FnOnce(&mut Job)) -> bool {
        let changed = {
            let mut state = self.0.state.lock().unwrap();
            let job = state.jobs.values_mut().find(|job| job.dir == dir);
            job.map(|job| {
                change(job);
                job.id.clone()
            })
        };
        let saved = changed.is_some_and(|id| self.persist(&id).is_ok());
        self.0.wake.notify_one();
        saved
    }

    /// Saves the job as it is now. Each save reads the job after it has the
    /// disk, so a file never goes back to an older state.
    fn persist(&self, id: &str) -> io::Result<()> {
        let mut links = self.0.disk.lock().unwrap();
        let job = self.0.state.lock().unwrap().jobs.get(id).cloned();
        match job {
            Some(job) => self.write(&mut links, &job),
            None => Ok(()),
        }
    }

    /// Writes the queue file, and the link file when its state changed.
    fn write(&self, links: &mut BTreeMap<String, &'static str>, job: &Job) -> io::Result<()> {
        let saved = write_private(
            &self.0.root.join(format!("{}.json", job.id)),
            &serde_json::to_vec(job)?,
        );
        let state = if job.cancelled {
            "removed"
        } else if job.error.is_some() {
            "failed"
        } else if job.ready {
            "ready"
        } else if job.finalized && job.quota != Quota::WithinLimits {
            "checking"
        } else {
            "uploading"
        };
        if (saved.is_err() && matches!(state, "ready" | "uploading"))
            || links.get(&job.id) == Some(&state)
            || !job.dir.is_dir()
        {
            return saved;
        }
        write_private(
            &job.dir.join("share.json"),
            &serde_json::to_vec(&Link {
                url: format!("{}/s/{}/", self.0.host, job.id),
                state: state.to_owned(),
                expires_at: job.created_at + 30 * 24 * 60 * 60,
            })?,
        )?;
        links.insert(job.id.clone(), state);
        saved
    }

    fn run(&self) {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(4)))
            .http_status_as_error(false)
            .build()
            .into();
        loop {
            let mut progress = false;
            for mut job in self.due() {
                if !job.dir.is_dir() {
                    job.cancelled = true;
                }
                progress |= self.step(&agent, &job);
            }
            if !progress {
                let state = self.0.state.lock().unwrap();
                let _ = self.0.wake.wait_timeout(state, Duration::from_millis(200));
            }
        }
    }

    /// The jobs whose turn has come. A cancel is due even while sharing is off.
    fn due(&self) -> Vec<Job> {
        let state = self.0.state.lock().unwrap();
        state
            .jobs
            .values()
            .filter(|job| {
                job.retry_at <= epoch()
                    && (job.cancelled || (state.enabled && !job.ready && job.error.is_none()))
            })
            .cloned()
            .collect()
    }

    /// One turn for a job, with its failure recorded. Returns whether it made progress.
    fn step(&self, agent: &ureq::Agent, job: &Job) -> bool {
        let error = match self.tick(agent, job) {
            Ok(worked) => return worked,
            Err(error) => error,
        };
        {
            let mut state = self.0.state.lock().unwrap();
            state.message.clone_from(&error.message);
            if let Some(current) = state.jobs.get_mut(&job.id) {
                current.attempts = current.attempts.saturating_add(1);
                current.retry_at = epoch() + (1_u64 << current.attempts.min(5));
                if error.permanent && !current.cancelled {
                    current.error = Some(error.message);
                }
            }
        }
        let _ = self.persist(&job.id);
        false
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
            let mut links = self.0.disk.lock().unwrap();
            {
                let mut state = self.0.state.lock().unwrap();
                state.jobs.remove(&job.id);
                state.message.clear();
            }
            links.remove(&job.id);
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
        let mut sent = Cow::Borrowed(&job.sent);
        let outcome = self.upload(agent, job, &mut sent);
        // The uploads of this turn reach the queue file in one write. A crash
        // before it sends them again, and the service accepts the same bytes twice.
        let saved = match sent {
            Cow::Owned(_) => self.persist(&job.id),
            Cow::Borrowed(_) => Ok(()),
        };
        let worked = outcome?;
        saved?;
        Ok(worked)
    }

    /// Sends what one walk of the take folder found, in dependency order, and
    /// publishes a finished take once nothing is left.
    fn upload(
        &self,
        agent: &ureq::Agent,
        job: &Job,
        sent: &mut Cow<Sent>,
    ) -> Result<bool, UploadError> {
        // Media can still arrive while the take records. Once it is finished,
        // a file it refers to and does not have will never come.
        let (folder, documents) = if job.finalized {
            let (folder, documents) = Folder::finished(&job.dir, sent)?;
            if job.quota == Quota::Unchecked {
                let quota = self.quota(job, &folder, &documents)?;
                self.update(&job.id, |job| job.quota = quota)?;
            }
            (folder, Some(documents))
        } else {
            (Folder::read(&job.dir, false)?, None)
        };
        let mut turn = Turn {
            started: Instant::now(),
            worked: false,
        };
        for (name, path) in &folder.media {
            let asset = self.media(job, name, path, sent)?;
            if !self.send(agent, job, &asset, sent, &mut turn)? {
                return Ok(turn.worked);
            }
        }
        for playlist in &folder.playlists {
            // A playlist can only refer to segments that have reached R2.
            if playlist.lists_only(|part| sent.contains_key(part))
                && !self.send(agent, job, &playlist.asset, sent, &mut turn)?
            {
                return Ok(turn.worked);
            }
        }
        let live = folder.live_manifest(|name| sent.contains_key(name))?;
        if !self.send(agent, job, &live, sent, &mut turn)? {
            return Ok(turn.worked);
        }
        let Some(documents) = documents else {
            return Ok(turn.worked);
        };
        for asset in &documents.assets {
            if !self.send(agent, job, asset, sent, &mut turn)? {
                return Ok(turn.worked);
            }
        }
        if !self.allowed(&job.id) {
            return Ok(turn.worked);
        }
        self.request(agent, "POST", job, "complete", &[])?;
        self.update(&job.id, |j| j.ready = true)?;
        let _ = std::fs::remove_dir_all(self.0.root.join(&job.id));
        Ok(true)
    }

    /// Uploads the asset unless the share already holds this version. `false`
    /// ends the turn: the job was paused or canceled, or its time is up.
    fn send(
        &self,
        agent: &ureq::Agent,
        job: &Job,
        asset: &Asset,
        sent: &mut Cow<Sent>,
        turn: &mut Turn,
    ) -> Result<bool, UploadError> {
        if sent.get(&asset.name) == Some(&asset.stamp) {
            return Ok(true);
        }
        if (turn.worked && turn.started.elapsed() >= TURN) || !self.allowed(&job.id) {
            return Ok(false);
        }
        let bytes = match &asset.source {
            Source::Bytes(bytes) => Cow::Borrowed(bytes.as_slice()),
            Source::File(path) => Cow::Owned(read_bounded(path)?),
        };
        self.request(agent, "PUT", job, &asset.name, &bytes)?;
        sent.to_mut()
            .insert(asset.name.clone(), asset.stamp.clone());
        self.change(&job.id, |j| {
            j.sent.insert(asset.name.clone(), asset.stamp.clone());
            j.uploaded_bytes
                .insert(asset.name.clone(), bytes.len() as u64);
        });
        turn.worked = true;
        Ok(true)
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut Job)) -> io::Result<()> {
        self.change(id, change);
        self.persist(id)
    }

    /// Changes the job in memory after a request succeeded.
    fn change(&self, id: &str, change: impl FnOnce(&mut Job)) {
        let mut state = self.0.state.lock().unwrap();
        state.message.clear();
        if let Some(job) = state.jobs.get_mut(id) {
            change(job);
            job.attempts = 0;
            job.retry_at = 0;
        }
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
        let mut response = match method {
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
            413 | 429 => {
                let detail: serde_json::Value =
                    serde_json::from_reader(response.body_mut().as_reader().take(4096))
                        .unwrap_or_default();
                let message = match (status, detail["error"].as_str()) {
                    (413, Some("share_files")) => FILE_LIMIT,
                    (413, Some("share_bytes")) => BYTE_LIMIT,
                    (413, Some("invalid_body")) => OBJECT_LIMIT,
                    (429, Some("installation_month")) => {
                        "Monthly allowance of 10 shares reached. Local recording saved."
                    }
                    (429, Some("ip_day" | "global_day")) => {
                        "Daily sharing allowance reached. Local recording saved. Retry tomorrow."
                    }
                    (429, _) => "Sharing allowance reached. Local recording saved. Retry later.",
                    _ => "Share exceeds the beta storage limit. Local recording saved.",
                };
                UploadError::permanent(message)
            }
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

    /// Checks the bytes the viewer will receive, including previously uploaded
    /// objects that have since disappeared from the take folder.
    fn quota(
        &self,
        job: &Job,
        folder: &Folder,
        documents: &Documents,
    ) -> Result<Quota, UploadError> {
        let mut sizes: BTreeMap<&str, Option<u64>> = job
            .sent
            .keys()
            .map(|name| (name.as_str(), job.uploaded_bytes.get(name).copied()))
            .collect();
        for (name, _) in &folder.media {
            sizes.entry(name).or_default();
        }
        for playlist in &folder.playlists {
            if playlist.lists_only(|part| sizes.contains_key(part)) {
                sizes.insert(&playlist.asset.name, Some(playlist.asset.len()?));
            }
        }
        let live = folder.live_manifest(|name| sizes.contains_key(name))?;
        for asset in documents.assets.iter().chain(std::iter::once(&live)) {
            sizes.insert(&asset.name, Some(asset.len()?));
        }
        if sizes.len() > MAX_SHARE_FILES {
            return Err(UploadError::permanent(FILE_LIMIT));
        }
        for (name, path) in &folder.media {
            let asset = self.media(job, name, path, &job.sent)?;
            let uploaded = job.sent.get(name) == Some(&asset.stamp);
            if uploaded && job.uploaded_bytes.contains_key(name) {
                continue;
            }
            match asset.len() {
                Ok(bytes) => {
                    sizes.insert(name, Some(bytes));
                }
                Err(error) if uploaded && error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut bytes = 0_u64;
        let mut unknown = 0;
        for size in sizes.into_values() {
            match size {
                Some(size) if size > MAX_OBJECT => {
                    return Err(UploadError::permanent(OBJECT_LIMIT))
                }
                Some(size) => bytes = bytes.saturating_add(size),
                None => unknown += 1,
            }
        }
        if bytes > MAX_SHARE_BYTES {
            return Err(UploadError::permanent(BYTE_LIMIT));
        }
        // Older queue files recorded stamps only. The server already accepted
        // each missing object under its per-file limit.
        Ok(if bytes + unknown * MAX_OBJECT <= MAX_SHARE_BYTES {
            Quota::WithinLimits
        } else {
            Quota::AwaitingServer
        })
    }

    /// The upload for one media file. A screenshot goes up as a JPEG.
    fn media(&self, job: &Job, name: &str, path: &Path, sent: &Sent) -> Result<Asset, UploadError> {
        let stamp = file_stamp(path)?;
        if path.extension().is_none_or(|extension| extension != "png") {
            return Ok(Asset {
                name: name.to_owned(),
                source: Source::File(path.to_owned()),
                stamp,
            });
        }
        let cache_dir = self
            .0
            .root
            .join(&job.id)
            .join(Path::new(name).with_extension(""));
        let cache = cache_dir.join(format!("{stamp}.jpg"));
        if sent.get(name) != Some(&stamp) && !cache.is_file() {
            std::fs::create_dir_all(&cache_dir)?;
            let stage = tempfile::Builder::new()
                .suffix(".jpg")
                .tempfile_in(&cache_dir)?;
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
                .arg(path)
                .arg("--out")
                .arg(stage.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            if !status.success() {
                return Err(UploadError::temporary(
                    "Could not compress screenshot. Retrying.",
                ));
            }
            stage.persist(&cache).map_err(|error| error.error)?;
        }
        Ok(Asset {
            name: name.to_owned(),
            source: Source::File(cache),
            stamp,
        })
    }
}

impl Asset {
    fn len(&self) -> io::Result<u64> {
        match &self.source {
            Source::Bytes(bytes) => Ok(bytes.len() as u64),
            Source::File(path) => Ok(path.metadata()?.len()),
        }
    }
}

impl Folder {
    fn read(dir: &Path, finalized: bool) -> io::Result<Self> {
        let mut files = Vec::new();
        collect_files(&dir.join("live"), &mut files)?;
        collect_files(&dir.join("shots"), &mut files)?;
        if finalized {
            collect_files(&dir.join("clips"), &mut files)?;
        }
        let mut folder = Folder {
            media: Vec::new(),
            playlists: Vec::new(),
        };
        for path in files {
            let name = path
                .strip_prefix(dir)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .into_owned();
            match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
                "m3u8" => {
                    let bytes = read_bounded(&path)?;
                    folder.playlists.push(Playlist {
                        parts: playlist_parts(&name, &bytes),
                        asset: bytes_asset(&name, bytes),
                    });
                }
                "png" if complete_png(&path) => {
                    let jpeg = name.strip_suffix(".png").unwrap().to_owned() + ".jpg";
                    folder.media.push((jpeg, path));
                }
                "mp4" | "m4s" | "jpg" => folder.media.push((name, path)),
                _ => {}
            }
        }
        // Immutable init/media objects precede manifests that reference them.
        folder
            .media
            .sort_by_cached_key(|(name, _)| (!name.ends_with("init.mp4"), name.clone()));
        folder
            .playlists
            .sort_by(|a, b| a.asset.name.cmp(&b.asset.name));
        Ok(folder)
    }

    /// Reads a finished take. Fails when it refers to media that can never
    /// reach the share: a file that is neither in the folder nor uploaded, or
    /// a playlist that lists one.
    fn finished(dir: &Path, sent: &Sent) -> Result<(Self, Documents), UploadError> {
        let folder = Folder::read(dir, true)?;
        let documents = documents(dir)?;
        let here: HashSet<&str> = folder.media.iter().map(|(name, _)| name.as_str()).collect();
        let reachable = |name: &str| sent.contains_key(name) || here.contains(name);
        let complete = documents.captured.as_ref().is_some_and(|captured| {
            captured.iter().all(|name| {
                let playlist = folder.playlists.iter().find(|p| p.asset.name == *name);
                playlist.map_or_else(
                    || reachable(name),
                    |playlist| playlist.lists_only(reachable),
                )
            })
        });
        if !complete {
            return Err(UploadError::permanent(MISSING));
        }
        Ok((folder, documents))
    }

    /// What the share page shows while the take records.
    fn live_manifest(&self, uploaded: impl Fn(&str) -> bool) -> serde_json::Result<Asset> {
        let clips: Vec<_> = self
            .playlists
            .iter()
            .filter(|playlist| uploaded(&playlist.asset.name))
            .map(|playlist| serde_json::json!({"playlist": playlist.asset.name}))
            .collect();
        let shots: Vec<_> = self
            .media
            .iter()
            .filter(|(name, _)| name.starts_with("shots/") && uploaded(name))
            .map(|(name, _)| serde_json::json!({"file": name}))
            .collect();
        Ok(bytes_asset(
            "live.json",
            serde_json::to_vec(&serde_json::json!({"clips": clips, "shots": shots}))?,
        ))
    }
}

impl Playlist {
    /// Whether it lists media and `known` holds for every file it lists.
    fn lists_only(&self, known: impl Fn(&str) -> bool) -> bool {
        let parts = self.parts.as_ref();
        parts.is_some_and(|parts| parts.iter().all(|part| known(part)))
    }
}

fn documents(dir: &Path) -> Result<Documents, UploadError> {
    let gone = |error: io::Error| match error.kind() {
        io::ErrorKind::NotFound => UploadError::permanent(MISSING),
        _ => error.into(),
    };
    let mut transcript: serde_json::Value =
        serde_json::from_slice(&read_bounded(&dir.join("transcript.json")).map_err(gone)?)?;
    let mut markdown = String::from_utf8(read_bounded(&dir.join("take.md")).map_err(gone)?)
        .map_err(io::Error::other)?;
    let mut captured = None;
    if let Some(captures) = transcript
        .get_mut("captures")
        .and_then(serde_json::Value::as_array_mut)
    {
        captured = Some(Vec::new());
        for capture in captures {
            if let Some(playlist) = capture
                .get("playlist")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
            {
                if let Some(video) = capture.get("video").and_then(serde_json::Value::as_str) {
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
            let media = capture.get("file").or_else(|| capture.get("video"));
            let frames = capture
                .get("frames")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .map(|frame| frame.get("file"));
            let named: Option<Vec<String>> = std::iter::once(media)
                .chain(frames)
                .map(|name| name?.as_str().map(str::to_owned))
                .collect();
            match (&mut captured, named) {
                (Some(all), Some(named)) => all.extend(named),
                _ => captured = None,
            }
        }
    }
    Ok(Documents {
        assets: [
            bytes_asset("take.md", markdown.into_bytes()),
            bytes_asset("transcript.json", serde_json::to_vec(&transcript)?),
        ],
        captured,
    })
}

/// The share URL in the text of a `share.json`, when a paste may use it: the
/// share is ready or uploading, has not expired, and lives on our host.
pub(crate) fn eligible_url(text: &str) -> Option<String> {
    let link: Link = serde_json::from_str(text).ok()?;
    let live = matches!(link.state.as_str(), "ready" | "uploading")
        && (link.expires_at == 0 || link.expires_at > epoch())
        && link.url.starts_with(&format!("{HOST}/s/"));
    live.then_some(link.url)
}

/// The paste is either the link or the local path, never both. The local
/// path returns when sharing fails or a link is revoked.
pub fn paste_link(markdown: &Path, local_paste: String) -> String {
    let link = markdown
        .parent()
        .and_then(|dir| std::fs::read_to_string(dir.join("share.json")).ok())
        .and_then(|text| eligible_url(&text));
    match link {
        // A queued upload finishes within seconds, so the paste is the link alone.
        Some(url) => local_paste.replace(&markdown.to_string_lossy().to_string(), &url),
        None => local_paste,
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

/// The init and media files a playlist lists. `None` until it lists any media.
fn playlist_parts(name: &str, bytes: &[u8]) -> Option<Vec<String>> {
    if !bytes.windows(8).any(|part| part == b"#EXTINF:") {
        return None;
    }
    let (parent, _) = name.rsplit_once('/')?;
    let parts = String::from_utf8_lossy(bytes)
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
        .map(|file| format!("{parent}/{file}"))
        .collect();
    Some(parts)
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

/// Seven base62 characters, the length of the share URL's last segment.
fn random_id() -> io::Result<String> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut file = std::fs::File::open("/dev/urandom")?;
    let mut id = String::new();
    let mut byte = [0_u8; 1];
    while id.len() < 7 {
        file.read_exact(&mut byte)?;
        // 248 is the largest multiple of 62 below 256, so the modulo stays unbiased.
        if byte[0] < 248 {
            id.push(ALPHABET[usize::from(byte[0] % 62)] as char);
        }
    }
    Ok(id)
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
