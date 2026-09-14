//! Signed update checks, downloads, and idle-only installation.

use std::io::{Read, Seek, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Sender},
    Arc, Mutex, MutexGuard,
};
use std::time::Duration;

use tauri_plugin_updater::{Update, UpdaterExt};

use crate::pill::{Notice, PillEvent};
use crate::session::Msg;

const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const INSTALL_PERMISSION_TIMEOUT: Duration = Duration::from_secs(2);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_ARTIFACT_BYTES: usize = 250 * 1024 * 1024;

#[derive(Clone)]
pub struct Updater {
    inner: Arc<Inner>,
}

struct Inner {
    inbox: Sender<Msg>,
    pill: Sender<PillEvent>,
    app: Mutex<Option<tauri::AppHandle>>,
    state: Mutex<State>,
    attached: AtomicBool,
}

enum State {
    Idle,
    Checking,
    Downloading,
    Ready(Box<CachedUpdate>),
    Installing,
}

struct CachedUpdate {
    update: Update,
    archive: std::fs::File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum View {
    Idle,
    Checking,
    Downloading,
    Ready(String),
    Installing,
}

#[derive(Clone, Copy)]
enum CheckKind {
    Background,
    Manual,
}

impl Updater {
    pub fn new(inbox: Sender<Msg>, pill: Sender<PillEvent>) -> Self {
        Self {
            inner: Arc::new(Inner {
                inbox,
                pill,
                app: Mutex::new(None),
                state: Mutex::new(State::Idle),
                attached: AtomicBool::new(false),
            }),
        }
    }

    pub fn attach(&self, app: &tauri::AppHandle) {
        if self.inner.attached.swap(true, Ordering::AcqRel) {
            return;
        }

        *lock(&self.inner.app) = Some(app.clone());
        self.start_check(CheckKind::Background);

        let updater = self.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(CHECK_INTERVAL);
            updater.start_check(CheckKind::Background);
        });
    }

    pub fn check(&self) {
        self.start_check(CheckKind::Manual);
    }

    pub fn install(&self) {
        let cached = {
            let mut state = lock(&self.inner.state);
            let previous = std::mem::replace(&mut *state, State::Installing);
            match previous {
                State::Ready(cached) => *cached,
                other => {
                    *state = other;
                    return;
                }
            }
        };

        let updater = self.clone();
        std::thread::spawn(move || updater.install_cached(cached));
    }

    pub fn label(&self) -> String {
        self.view().label()
    }

    pub fn ready(&self) -> bool {
        matches!(*lock(&self.inner.state), State::Ready(_))
    }

    fn start_check(&self, kind: CheckKind) {
        let Some(app) = lock(&self.inner.app).clone() else {
            if matches!(kind, CheckKind::Manual) {
                self.info("Updater is not ready");
            }
            return;
        };

        if !lock(&self.inner.state).begin_check() {
            return;
        }

        let updater = self.clone();
        tauri::async_runtime::spawn(async move {
            updater.check_and_download(app, kind).await;
        });
    }

    async fn check_and_download(&self, app: tauri::AppHandle, kind: CheckKind) {
        let result = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
            Ok(updater) => updater.check().await,
            Err(error) => {
                self.finish_check_error(kind, error);
                return;
            }
        };

        let mut update = match result {
            Ok(Some(update)) => update,
            Ok(None) | Err(tauri_plugin_updater::Error::ReleaseNotFound) => {
                *lock(&self.inner.state) = State::Idle;
                if matches!(kind, CheckKind::Manual) {
                    self.info("No update available");
                }
                return;
            }
            Err(error) => {
                self.finish_check_error(kind, error);
                return;
            }
        };

        *lock(&self.inner.state) = State::Downloading;
        update.timeout = Some(DOWNLOAD_TIMEOUT);

        let mut reported_too_large = false;
        let download = update
            .download(
                |_, content_length| {
                    reported_too_large |=
                        content_length.is_some_and(|length| length > MAX_ARTIFACT_BYTES as u64);
                },
                || {},
            )
            .await;

        match download {
            Ok(bytes) if reported_too_large || bytes.len() > MAX_ARTIFACT_BYTES => {
                *lock(&self.inner.state) = State::Idle;
                if matches!(kind, CheckKind::Manual) {
                    self.info("Update download is too large");
                }
            }
            Ok(bytes) => match cache_archive(&bytes) {
                Ok(archive) => {
                    *lock(&self.inner.state) =
                        State::Ready(Box::new(CachedUpdate { update, archive }))
                }
                Err(_) => {
                    *lock(&self.inner.state) = State::Idle;
                    if matches!(kind, CheckKind::Manual) {
                        self.info("Could not save update download");
                    }
                }
            },
            Err(_) => {
                *lock(&self.inner.state) = State::Idle;
                if matches!(kind, CheckKind::Manual) {
                    self.info("Could not download update");
                }
            }
        }
    }

    fn finish_check_error(&self, kind: CheckKind, _error: tauri_plugin_updater::Error) {
        *lock(&self.inner.state) = State::Idle;
        if matches!(kind, CheckKind::Manual) {
            self.info("Could not check for updates");
        }
    }

    fn install_cached(&self, mut cached: CachedUpdate) {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self.inner.inbox.send(Msg::PrepareUpdate(reply_tx)).is_err() {
            self.restore_ready(cached);
            self.info("Could not prepare the update");
            return;
        }

        match reply_rx.recv_timeout(INSTALL_PERMISSION_TIMEOUT) {
            Ok(true) => {}
            Ok(false) => {
                self.restore_ready(cached);
                self.info("Finish the recording before updating");
                return;
            }
            Err(_) => {
                let _ = self.inner.inbox.send(Msg::UpdateAborted);
                self.restore_ready(cached);
                self.info("Could not prepare the update");
                return;
            }
        }

        let mut bytes = Vec::new();
        if cached
            .archive
            .rewind()
            .and_then(|_| cached.archive.read_to_end(&mut bytes))
            .is_err()
        {
            let _ = self.inner.inbox.send(Msg::UpdateAborted);
            self.restore_ready(cached);
            self.info("Could not read the downloaded update");
            return;
        }
        if let Err(error) = cached.update.install(&bytes) {
            let _ = self.inner.inbox.send(Msg::UpdateAborted);
            self.restore_ready(cached);
            self.info(format!("Could not install update: {error}"));
            return;
        }

        let Some(app) = lock(&self.inner.app).clone() else {
            let _ = self.inner.inbox.send(Msg::UpdateAborted);
            self.restore_ready(cached);
            self.info("Could not restart after updating");
            return;
        };
        app.restart();
    }

    fn restore_ready(&self, cached: CachedUpdate) {
        *lock(&self.inner.state) = State::Ready(Box::new(cached));
    }

    fn info(&self, text: impl Into<String>) {
        let _ = self
            .inner
            .pill
            .send(PillEvent::Flash(Notice::Info(text.into())));
    }

    fn view(&self) -> View {
        match &*lock(&self.inner.state) {
            State::Idle => View::Idle,
            State::Checking => View::Checking,
            State::Downloading => View::Downloading,
            State::Ready(cached) => View::Ready(cached.update.version.clone()),
            State::Installing => View::Installing,
        }
    }
}

/// An unlinked private file releases download RAM and disappears on exit.
/// Keeping its descriptor prevents another process from replacing verified bytes by path.
fn cache_archive(bytes: &[u8]) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let root = crate::paths::app_support();
    std::fs::create_dir_all(&root)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = root.join(format!(".update-{}-{stamp}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(path)?;
    file.write_all(bytes)?;
    file.rewind()?;
    Ok(file)
}

impl State {
    fn begin_check(&mut self) -> bool {
        if !matches!(self, Self::Idle) {
            return false;
        }
        *self = Self::Checking;
        true
    }
}

impl View {
    fn label(self) -> String {
        match self {
            Self::Idle => "Check for updates".to_owned(),
            Self::Checking => "Checking for updates".to_owned(),
            Self::Downloading => "Downloading update".to_owned(),
            Self::Ready(version) => format!("Restart to update {version}"),
            Self::Installing => "Installing update".to_owned(),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::{State, View};

    #[test]
    fn only_idle_state_starts_a_check() {
        let mut state = State::Idle;
        assert!(state.begin_check());
        assert!(!state.begin_check());

        let mut state = State::Downloading;
        assert!(!state.begin_check());

        let mut state = State::Installing;
        assert!(!state.begin_check());
    }

    #[test]
    fn labels_match_tray_actions() {
        assert_eq!(View::Idle.label(), "Check for updates");
        assert_eq!(View::Checking.label(), "Checking for updates");
        assert_eq!(View::Downloading.label(), "Downloading update");
        assert_eq!(
            View::Ready("0.1.1".to_owned()).label(),
            "Restart to update 0.1.1"
        );
        assert_eq!(View::Installing.label(), "Installing update");
    }
}
