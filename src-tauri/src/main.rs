//! Process entry point and actor wiring.

mod cli;
mod clip;
mod config;
mod engine;
mod history;
mod hotkeys;
mod menu;
mod mic;
mod paste;
mod pill;
mod qos;
mod recorder;
mod rivals;
mod session;
mod share;
mod tray;
mod trigger;

fn acquire_single_instance() -> std::fs::File {
    use std::os::fd::AsRawFd;

    let data = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let app_dir = data.join("see.computer");
    if let Err(error) = std::fs::create_dir_all(&app_dir) {
        eprintln!("could not create app data directory: {error}");
        std::process::exit(1);
    }
    let path = app_dir.join("instance.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|error| {
            eprintln!("could not open instance lock: {error}");
            std::process::exit(1);
        });
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return file;
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        eprintln!("see.computer is already running");
        std::process::exit(0);
    }
    eprintln!("could not lock instance file: {error}");
    std::process::exit(1);
}

fn main() {
    if let Some(cmd) = cli::parse(std::env::args().skip(1)) {
        std::process::exit(cli::run(cmd));
    }

    let _instance = acquire_single_instance();
    let mic = match std::env::var_os("SEE_COMPUTER_AUDIO_FILE") {
        Some(path) => mic::Source::Replay(path.into()),
        None => mic::Source::Default,
    };
    let (tx, rx) = std::sync::mpsc::channel::<session::Msg>();
    let (pill_tx, pill_rx) = std::sync::mpsc::channel::<pill::PillEvent>();
    let tray_pill_tx = pill_tx.clone();
    let config = config::Config::load();
    let trigger = std::sync::Arc::new(std::sync::Mutex::new(config.trigger));
    let history = history::History::start(config.history);
    let status = std::sync::Arc::new(std::sync::Mutex::new(session::EngineStatus::Loading {
        phase: engine::Phase::Downloading,
        pct: None,
    }));
    let rivals = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    rivals::spawn(pill_tx.clone(), rivals.clone());
    let controller = session::spawn(
        session::Wiring {
            mic,
            engine: engine::Loader::Models(engine::Models::default_root()),
            recorder: recorder::Recorder::screencapture(recorder::default_dir()),
            share: share::Share::LocalFile,
            paste: paste::Paste::system(),
            pill: pill_tx,
            trail: session::Trail::from_env(),
            history: history.clone(),
            status: status.clone(),
        },
        (tx.clone(), rx),
    );

    let setup_tx = tx.clone();
    let setup_watcher_tx = tx.clone();
    let setup_trigger = trigger.clone();
    let setup_history = history;
    let setup_status = status;
    let setup_rivals = rivals;
    let app = tauri::Builder::default()
        .plugin(hotkeys::plugin(tx.clone()))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            pill::attach(app.handle(), pill_rx);
            tray::install(
                app.handle(),
                setup_tx,
                setup_trigger.clone(),
                tray_pill_tx,
                setup_history,
                setup_status,
                setup_rivals,
            )?;
            let selected = *setup_trigger
                .lock()
                .expect("trigger mutex poisoned at startup");
            if let Err(error) = hotkeys::set_chords_registered(app.handle(), !selected.uses_tap()) {
                eprintln!("hotkey unavailable at startup: {error}");
            }
            paste::accessibility_trusted(true);
            trigger::spawn_watcher(setup_trigger, setup_watcher_tx);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("see.computer failed to start");
    let mut controller = Some(controller);
    app.run(move |_, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let _ = tx.send(session::Msg::Quit);
            if let Some(handle) = controller.take() {
                let _ = handle.join();
            }
        }
    });
}
