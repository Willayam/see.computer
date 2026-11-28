//! Opt-in, disk-backed uploads. Capture and transcription never wait for HTTP.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HOST: &str = "https://see.computer";
const MAX_OBJECT: u64 = 8 * 1024 * 1024;
const MAX_SHARE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHARE_FILES: usize = 1500;
const RETENTION_SECS: u64 = 30 * 24 * 60 * 60;
const FILE_LIMIT: &str = "Share exceeds 1,500 files. Local recording saved.";
const BYTE_LIMIT: &str = "Share exceeds 256 MiB. Local recording saved.";
const OBJECT_LIMIT: &str = "A recording file exceeds 8 MiB. Local recording saved.";
const UPLOAD_TURN: Duration = Duration::from_secs(1);
const FULL_SCAN_INTERVAL: Duration = Duration::from_secs(5);
const MISSING: &str = "A recording file is missing. The share is incomplete.";

type UploadedStamps = BTreeMap<String, String>;

#[derive(Clone)]
pub struct Sharing(Arc<Shared>);

struct Shared {
    root: PathBuf,
    host: String,
    state: Mutex<State>,
    wake: Condvar,
    /// When both locks are needed, take this one first and release `state` before file I/O.
    queue_files: Mutex<BTreeMap<String, &'static str>>,
}

struct State {
    enabled: bool,
    jobs: BTreeMap<String, Job>,
    message: String,
    idle: BTreeMap<String, Arc<LiveInventory>>,
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
    sent: UploadedStamps,
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
    #[serde(default)]
    timing: UploadTiming,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct UploadTiming {
    started_ms: Option<u64>,
    first_playable_ms: Option<u64>,
    published_ms: Option<u64>,
    last_error_ms: Option<u64>,
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

struct Folder {
    /// HLS initialization segments precede their media fragments.
    media: Vec<(String, PathBuf)>,
    playlists: Vec<Playlist>,
    live: Option<LiveInventory>,
}

struct LiveInventory {
    scanned: Instant,
    paths: Vec<(PathBuf, Option<FileStamp>)>,
}

#[derive(PartialEq)]
struct FileStamp {
    inode: u64,
    len: u64,
    modified: SystemTime,
}

impl LiveInventory {
    fn stamp(path: &Path) -> io::Result<Option<FileStamp>> {
        use std::os::unix::fs::MetadataExt;
        match path.metadata() {
            Ok(meta) => Ok(Some(FileStamp {
                inode: meta.ino(),
                len: meta.len(),
                modified: meta.modified()?,
            })),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn observe(&mut self, path: &Path) -> io::Result<()> {
        self.paths.push((path.to_owned(), Self::stamp(path)?));
        Ok(())
    }

    fn unchanged(&self) -> bool {
        // Renaming a fragment changes its directory stamp. An in-place external edit does not.
        self.scanned.elapsed() < FULL_SCAN_INTERVAL
            && self
                .paths
                .iter()
                .all(|(path, stamp)| Self::stamp(path).is_ok_and(|value| value == *stamp))
    }
}

struct Playlist {
    asset: Asset,
    parts: Option<Vec<String>>,
}

struct LiveManifest {
    asset: Asset,
    lists_clip: bool,
}

struct Documents {
    assets: [Asset; 2],
    captured: Option<Vec<String>>,
}

struct Turn {
    started: Instant,
    uploaded: bool,
}

enum Batch {
    Pending,
    ReadyToPublish,
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
        let mut links = BTreeMap::new();
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
                        let _ = job.save(&root);
                    } else {
                        job.cancelled = true;
                    }
                }
                if let Ok(Some(state)) = job.reconcile_link(host) {
                    links.insert(job.id.clone(), state);
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
                idle: BTreeMap::new(),
            }),
            wake: Condvar::new(),
            queue_files: Mutex::new(links),
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
        let mut links = self.0.queue_files.lock().unwrap();
        {
            let state = self.0.state.lock().unwrap();
            if !state.enabled || state.jobs.values().any(|job| job.dir == dir) {
                return;
            }
        }
        let result = (|| -> io::Result<Job> {
            let started_ms = epoch_millis();
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
                created_at: started_ms / 1000,
                timing: UploadTiming {
                    started_ms: Some(started_ms),
                    ..Default::default()
                },
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

    /// Returns the paste for a finished take, using its local path if the share cannot be verified.
    pub fn finalize_paste(&self, markdown: &Path, local_paste: String) -> String {
        let Some(dir) = markdown.parent() else {
            return local_paste;
        };
        let job = {
            let state = self.0.state.lock().unwrap();
            state.jobs.values().find(|job| job.dir == dir).cloned()
        };
        let Some(job) = job else { return local_paste };
        let checked = Folder::finished(dir, &job.sent)
            .and_then(|(folder, documents)| self.quota(&job, &folder, &documents));
        let check_failed = checked.is_err();
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
            if check_failed {
                job.timing.last_error_ms = Some(epoch_millis());
            }
            job.error = job.error.take().or(failure);
            eligible = !job.cancelled && job.error.is_none() && job.quota == Quota::WithinLimits;
        });
        if eligible && saved {
            paste_link(markdown, local_paste)
        } else {
            local_paste
        }
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
            state.idle.clear();
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
            let changed = job.map(|job| {
                change(job);
                job.id.clone()
            });
            if let Some(id) = &changed {
                state.idle.remove(id);
            }
            changed
        };
        let saved = changed.is_some_and(|id| self.persist(&id).is_ok());
        self.0.wake.notify_one();
        saved
    }

    fn persist(&self, id: &str) -> io::Result<()> {
        let mut links = self.0.queue_files.lock().unwrap();
        let job = self.0.state.lock().unwrap().jobs.get(id).cloned();
        match job {
            Some(job) => self.write(&mut links, &job),
            None => Ok(()),
        }
    }

    fn write(&self, links: &mut BTreeMap<String, &'static str>, job: &Job) -> io::Result<()> {
        let saved = job.save(&self.0.root);
        let state = job.link_state();
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
                expires_at: job.expires_at(),
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
                progress |= self.step(&agent, job);
            }
            if !progress {
                let state = self.0.state.lock().unwrap();
                match state.next_wake(epoch()) {
                    None => drop(self.0.wake.wait(state).unwrap()),
                    Some(delay) if !delay.is_zero() => {
                        drop(self.0.wake.wait_timeout(state, delay).unwrap())
                    }
                    Some(_) => {}
                }
            }
        }
    }

    fn due(&self) -> Vec<Job> {
        let now = epoch();
        let candidates: Vec<_> = {
            let state = self.0.state.lock().unwrap();
            state
                .jobs
                .values()
                .filter(|job| job.retry_at <= now && job.runnable(state.enabled))
                .map(|job| {
                    let inventory = if job.finalized || job.cancelled {
                        None
                    } else {
                        state.idle.get(&job.id).cloned()
                    };
                    (job.id.clone(), inventory)
                })
                .collect()
        };
        candidates
            .into_iter()
            .filter_map(|(id, inventory)| {
                if inventory.is_some_and(|live| live.unchanged()) {
                    return None;
                }
                let state = self.0.state.lock().unwrap();
                let job = state.jobs.get(&id)?;
                (job.retry_at <= now && job.runnable(state.enabled)).then(|| job.clone())
            })
            .collect()
    }

    fn step(&self, agent: &ureq::Agent, mut job: Job) -> bool {
        let error = match self.tick(agent, &mut job) {
            Ok(worked) => return worked,
            Err(error) => error,
        };
        {
            let mut state = self.0.state.lock().unwrap();
            state.message.clone_from(&error.message);
            if let Some(current) = state.jobs.get_mut(&job.id) {
                current.timing.last_error_ms = Some(epoch_millis());
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

    fn tick(&self, agent: &ureq::Agent, job: &mut Job) -> Result<bool, UploadError> {
        if job.cancelled {
            // DELETE is retried even while sharing is off. It never uploads content.
            // Always send it: a create response may have been lost before local persistence.
            self.request(agent, "DELETE", job, "", &[])?;
            let mut links = self.0.queue_files.lock().unwrap();
            job.reconcile_link(&self.0.host)?;
            {
                let mut state = self.0.state.lock().unwrap();
                state.jobs.remove(&job.id);
                state.idle.remove(&job.id);
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
        let mut turn = Turn {
            started: Instant::now(),
            uploaded: false,
        };
        let batch = self.upload(agent, job, &mut turn);
        // Once the service accepts completion, it refuses further PUTs. A restarted
        // client needs the acknowledged stamps on disk before making that request.
        let checkpoint = if turn.uploaded || matches!(batch, Ok(Batch::ReadyToPublish)) {
            self.persist(&job.id)
        } else {
            Ok(())
        };
        let batch = batch?;
        checkpoint?;
        if matches!(batch, Batch::ReadyToPublish) && self.allowed(&job.id) {
            self.request(agent, "POST", job, "complete", &[])?;
            if let Err(error) = self.update(&job.id, |j| {
                j.ready = true;
                j.timing.published_ms.get_or_insert_with(epoch_millis);
            }) {
                self.change(&job.id, |j| j.ready = false);
                return Err(error.into());
            }
            let _ = std::fs::remove_dir_all(self.0.root.join(&job.id));
            return Ok(true);
        }
        Ok(turn.uploaded)
    }

    fn upload(
        &self,
        agent: &ureq::Agent,
        job: &mut Job,
        turn: &mut Turn,
    ) -> Result<Batch, UploadError> {
        let (folder, documents) = if job.finalized {
            let (folder, documents) = Folder::finished(&job.dir, &job.sent)?;
            if job.quota == Quota::Unchecked {
                let quota = self.quota(job, &folder, &documents)?;
                self.update(&job.id, |job| job.quota = quota)?;
            }
            (folder, Some(documents))
        } else {
            (Folder::read(&job.dir, false)?, None)
        };
        for (name, path) in &folder.media {
            let asset = self.media(job, name, path)?;
            if self.send(agent, job, &asset, turn)?.is_break() {
                return Ok(Batch::Pending);
            }
        }
        for playlist in &folder.playlists {
            // A playlist can only refer to segments that have reached R2.
            if playlist.lists_only(|part| job.sent.contains_key(part))
                && self.send(agent, job, &playlist.asset, turn)?.is_break()
            {
                return Ok(Batch::Pending);
            }
        }
        let live = folder.live_manifest(|name| job.sent.contains_key(name))?;
        if self.send(agent, job, &live.asset, turn)?.is_break() {
            return Ok(Batch::Pending);
        }
        if live.lists_clip
            && job.timing.started_ms.is_some()
            && job.timing.first_playable_ms.is_none()
        {
            self.change(&job.id, |j| {
                j.timing.first_playable_ms.get_or_insert_with(epoch_millis);
            });
        }
        let Some(documents) = documents else {
            if let Some(inventory) = folder.live {
                let mut state = self.0.state.lock().unwrap();
                if state
                    .jobs
                    .get(&job.id)
                    .is_some_and(|job| !job.finalized && !job.cancelled)
                {
                    state.idle.insert(job.id.clone(), Arc::new(inventory));
                }
            }
            return Ok(Batch::Pending);
        };
        for asset in &documents.assets {
            if self.send(agent, job, asset, turn)?.is_break() {
                return Ok(Batch::Pending);
            }
        }
        Ok(Batch::ReadyToPublish)
    }

    fn send(
        &self,
        agent: &ureq::Agent,
        job: &mut Job,
        asset: &Asset,
        turn: &mut Turn,
    ) -> Result<ControlFlow<()>, UploadError> {
        if job.sent.get(&asset.name) == Some(&asset.stamp) {
            return Ok(ControlFlow::Continue(()));
        }
        if (turn.uploaded && turn.started.elapsed() >= UPLOAD_TURN) || !self.allowed(&job.id) {
            return Ok(ControlFlow::Break(()));
        }
        let bytes = match &asset.source {
            Source::Bytes(bytes) => Cow::Borrowed(bytes.as_slice()),
            Source::File(path) => Cow::Owned(read_bounded(path)?),
        };
        self.request(agent, "PUT", job, &asset.name, &bytes)?;
        job.sent.insert(asset.name.clone(), asset.stamp.clone());
        self.change(&job.id, |j| {
            j.sent.insert(asset.name.clone(), asset.stamp.clone());
            j.uploaded_bytes
                .insert(asset.name.clone(), bytes.len() as u64);
        });
        turn.uploaded = true;
        Ok(ControlFlow::Continue(()))
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut Job)) -> io::Result<()> {
        self.change(id, change);
        self.persist(id)
    }

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
        for asset in documents.assets.iter().chain(std::iter::once(&live.asset)) {
            sizes.insert(&asset.name, Some(asset.len()?));
        }
        if sizes.len() > MAX_SHARE_FILES {
            return Err(UploadError::permanent(FILE_LIMIT));
        }
        for (name, path) in &folder.media {
            let asset = self.media(job, name, path)?;
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
        let mut legacy_objects_without_sizes = 0;
        for size in sizes.into_values() {
            match size {
                Some(size) if size > MAX_OBJECT => {
                    return Err(UploadError::permanent(OBJECT_LIMIT))
                }
                Some(size) => bytes = bytes.saturating_add(size),
                None => legacy_objects_without_sizes += 1,
            }
        }
        if bytes > MAX_SHARE_BYTES {
            return Err(UploadError::permanent(BYTE_LIMIT));
        }
        Ok(
            if bytes + legacy_objects_without_sizes * MAX_OBJECT <= MAX_SHARE_BYTES {
                Quota::WithinLimits
            } else {
                Quota::AwaitingServer
            },
        )
    }

    fn media(&self, job: &Job, name: &str, path: &Path) -> Result<Asset, UploadError> {
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
        if job.sent.get(name) != Some(&stamp) && !cache.is_file() {
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

impl State {
    fn next_wake(&self, now: u64) -> Option<Duration> {
        self.jobs
            .values()
            .filter(|job| job.runnable(self.enabled))
            .map(|job| {
                if job.retry_at > now {
                    Duration::from_secs(job.retry_at - now)
                } else if job.created && !job.finalized && !job.cancelled {
                    Duration::from_millis(200)
                } else {
                    Duration::ZERO
                }
            })
            .min()
    }
}

impl Job {
    fn expires_at(&self) -> u64 {
        if self.created_at == 0 {
            0
        } else {
            self.created_at.saturating_add(RETENTION_SECS)
        }
    }

    fn save(&self, root: &Path) -> io::Result<()> {
        write_private(
            &root.join(format!("{}.json", self.id)),
            &serde_json::to_vec(self)?,
        )
    }

    fn link_state(&self) -> &'static str {
        if self.cancelled {
            "removed"
        } else if self.error.is_some() {
            "failed"
        } else if self.ready {
            "ready"
        } else if self.finalized && self.quota != Quota::WithinLimits {
            "checking"
        } else {
            "uploading"
        }
    }

    fn reconcile_link(&self, host: &str) -> io::Result<Option<&'static str>> {
        let state = self.link_state();
        let expires = self.expires_at();
        if state == "uploading" || (state == "ready" && expires > 0 && expires <= epoch()) {
            return Ok(None);
        }
        let path = self.dir.join("share.json");
        let bytes = match std::fs::read(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            result => result?,
        };
        let Ok(mut link) = serde_json::from_slice::<Link>(&bytes) else {
            return Ok(None);
        };
        if link.url != format!("{host}/s/{}/", self.id) {
            return Ok(None);
        }
        if link.state != state {
            if link.state == "removed"
                || (link.state == "failed" && state != "removed")
                || (state == "ready" && !matches!(link.state.as_str(), "uploading" | "checking"))
            {
                return Ok(None);
            }
            link.state = state.to_owned();
            write_private(&path, &serde_json::to_vec(&link)?)?;
        }
        Ok(Some(state))
    }

    fn runnable(&self, enabled: bool) -> bool {
        self.cancelled || (enabled && !self.ready && self.error.is_none())
    }
}

impl Folder {
    fn read(dir: &Path, finalized: bool) -> io::Result<Self> {
        let mut files = Vec::new();
        let mut live = (!finalized).then(|| LiveInventory {
            scanned: Instant::now(),
            paths: Vec::new(),
        });
        collect_files(&dir.join("live"), &mut files, &mut live)?;
        collect_files(&dir.join("shots"), &mut files, &mut live)?;
        if finalized {
            collect_files(&dir.join("clips"), &mut files, &mut live)?;
        }
        let mut folder = Folder {
            media: Vec::new(),
            playlists: Vec::new(),
            live,
        };
        for path in files {
            let name = path
                .strip_prefix(dir)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .into_owned();
            let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(extension, "png" | "m3u8") {
                if let Some(live) = &mut folder.live {
                    live.observe(&path)?;
                }
            }
            match extension {
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

    fn finished(dir: &Path, sent: &UploadedStamps) -> Result<(Self, Documents), UploadError> {
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

    fn live_manifest(&self, uploaded: impl Fn(&str) -> bool) -> serde_json::Result<LiveManifest> {
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
        Ok(LiveManifest {
            lists_clip: !clips.is_empty(),
            asset: bytes_asset(
                "live.json",
                serde_json::to_vec(&serde_json::json!({"clips": clips, "shots": shots}))?,
            ),
        })
    }
}

impl Playlist {
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
    let markdown = String::from_utf8(read_bounded(&dir.join("take.md")).map_err(gone)?)
        .map_err(io::Error::other)?;
    let mut links = BTreeMap::new();
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
                    links.insert(video.to_owned(), playlist.clone());
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
                    links.insert(file, jpeg.clone());
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
            bytes_asset("take.md", capture_links(markdown, &links).into_bytes()),
            bytes_asset("transcript.json", serde_json::to_vec(&transcript)?),
        ],
        captured,
    })
}

fn starts_spoken_segment(line: &str) -> bool {
    let Some((range, _)) = line
        .strip_prefix("**")
        .and_then(|line| line.split_once("** "))
    else {
        return false;
    };
    range.split_once(['–', '-']).is_some_and(|(start, end)| {
        [start, end].into_iter().all(|time| {
            time.contains(':')
                && time
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b':')
        })
    })
}

fn capture_links(markdown: String, links: &BTreeMap<String, String>) -> String {
    if links.is_empty() {
        return markdown;
    }
    let mut remote = String::with_capacity(markdown.len());
    let mut narration = false;
    for line in markdown.split_inclusive('\n') {
        if line.trim().is_empty() {
            narration = false;
        } else if starts_spoken_segment(line) {
            narration = true;
        }
        if narration || !line.contains("](") {
            remote.push_str(line);
            continue;
        }
        let mut parts = line.split("](");
        remote.push_str(parts.next().unwrap_or_default());
        for part in parts {
            remote.push_str("](");
            if let Some((path, tail)) = part.split_once(')') {
                remote.push_str(links.get(path).map(String::as_str).unwrap_or(path));
                remote.push(')');
                remote.push_str(tail);
            } else {
                remote.push_str(part);
            }
        }
    }
    remote
}

/// Returns a share link that is uploading or ready on our host and has yet to expire.
pub(crate) fn eligible_url(text: &str) -> Option<String> {
    let link: Link = serde_json::from_str(text).ok()?;
    let live = matches!(link.state.as_str(), "ready" | "uploading")
        && (link.expires_at == 0 || link.expires_at > epoch())
        && link.url.starts_with(&format!("{HOST}/s/"));
    live.then_some(link.url)
}

/// Substitutes an eligible share link for the attachment path, preserving the narration.
pub fn paste_link(markdown: &Path, mut local_paste: String) -> String {
    let link = markdown
        .parent()
        .and_then(|dir| std::fs::read_to_string(dir.join("share.json")).ok())
        .and_then(|text| eligible_url(&text));
    if let Some(url) = link {
        if let Some(prefix) = local_paste.strip_suffix(markdown.to_string_lossy().as_ref()) {
            local_paste.replace_range(prefix.len().., &url);
        }
    }
    local_paste
}

fn collect_files(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    live: &mut Option<LiveInventory>,
) -> io::Result<()> {
    if let Some(live) = live {
        live.observe(dir)?;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(&entry.path(), files, live)?;
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn playlist_parts(name: &str, bytes: &[u8]) -> Option<Vec<String>> {
    let (parent, _) = name.rsplit_once('/')?;
    let mut parts = Vec::new();
    let mut needs_uri = false;
    let mut has_media = false;
    for line in std::str::from_utf8(bytes).ok()?.lines().map(str::trim) {
        if line.starts_with("#EXTINF:") {
            if needs_uri {
                return None;
            }
            needs_uri = true;
        } else if let Some(map) = line.strip_prefix("#EXT-X-MAP:") {
            let (_, uri) = map.split_once("URI=\"")?;
            let (file, _) = uri.split_once('"')?;
            if file.is_empty() {
                return None;
            }
            parts.push(format!("{parent}/{file}"));
        } else if !line.starts_with('#') && !line.is_empty() {
            if !needs_uri {
                return None;
            }
            needs_uri = false;
            has_media = true;
            parts.push(format!("{parent}/{line}"));
        }
    }
    (has_media && !needs_uri).then_some(parts)
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
    epoch_millis() / 1000
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
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
