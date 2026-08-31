//! Process entry point and actor wiring.

mod boost;
mod cli;
mod clip;
mod config;
mod engine;
mod filler;
mod history;
mod hotkeys;
mod menu;
mod mic;
mod paste;
mod paths;
mod pill;
mod qos;
mod recents;
mod recorder;
mod rivals;
mod session;
mod text;
mod tray;
mod trigger;

fn acquire_single_instance() -> std::fs::File {
    use std::os::fd::AsRawFd;

    if let Err(error) = std::fs::create_dir_all(paths::app_support()) {
        eprintln!("could not create app data directory: {error}");
        std::process::exit(1);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths::instance_lock())
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
    let gesture = std::sync::Arc::new(trigger::Gesture::default());
    let history = history::History::start(config.history);
    let status = std::sync::Arc::new(std::sync::Mutex::new(session::EngineStatus::Loading {
        phase: engine::Phase::Downloading,
        pct: None,
    }));
    let rivals = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    rivals::spawn(rivals.clone());
    let controller = session::spawn(
        session::Wiring {
            mic,
            engine: engine::Loader::Models(engine::Models::default_root()),
            recorder: recorder::Recorder::screencapture(paths::documents()),
            paste: paste::Paste::system(),
            pill: pill_tx,
            trail: session::Trail::from_env(),
            history: history.clone(),
            status: status.clone(),
            gesture: gesture.clone(),
        },
        (tx.clone(), rx),
    );

    let setup_tx = tx.clone();
    let setup_watcher_tx = tx.clone();
    let setup_trigger = trigger.clone();
    let setup_gesture = gesture;
    let setup_history = history;
    let setup_status = status;
    let app = tauri::Builder::default()
        .plugin(hotkeys::plugin(tx.clone()))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let cancel_app = app.handle().clone();
            pill::attach(app.handle(), pill_rx, move |armed| {
                let app = cancel_app.clone();
                let _ =
                    cancel_app.run_on_main_thread(move || hotkeys::set_cancel_armed(&app, armed));
            });
            tray::install(
                app.handle(),
                setup_tx,
                setup_trigger.clone(),
                tray_pill_tx,
                setup_history,
                setup_status,
            )?;
            paste::accessibility_trusted(true);
            trigger::spawn_watcher(setup_trigger, setup_watcher_tx, setup_gesture);
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
