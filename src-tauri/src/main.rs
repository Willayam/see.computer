//! Process entry point and actor wiring.

mod boost;
mod cli;
mod clip;
mod config;
mod dictionary;
mod engine;
mod eval;
mod filler;
mod fix;
mod history;
mod hotkeys;
mod learn;
mod ledger;
mod menu;
mod mic;
mod numbers;
mod paste;
mod paths;
mod pill;
mod qos;
mod readback;
mod recents;
mod recorder;
mod rivals;
mod session;
mod sharing;
mod target;
mod text;
mod tray;
mod trigger;
mod updater;
mod vocabulary;

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
    let ledger = ledger::Ledger::start(history.enabled_flag(), Some(pill_tx.clone()));
    let readback = readback::Readback::start(ledger.clone());
    let sharing = sharing::Sharing::start(config.sharing);
    let updater = updater::Updater::new(tx.clone(), pill_tx.clone());
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
            paste: paste::Paste::system(ledger.clone(), readback),
            pill: pill_tx,
            trail: session::Trail::from_env(),
            history: history.clone(),
            ledger: ledger.clone(),
            sharing: sharing.clone(),
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
    let setup_ledger = ledger;
    let setup_status = status;
    let dictionary = dictionary::Dictionary::new(setup_ledger.clone(), setup_history.clone());
    let app = tauri::Builder::default()
        .manage(dictionary)
        .invoke_handler(tauri::generate_handler![
            dictionary::dictionary_load,
            dictionary::dictionary_stamp,
            dictionary::dictionary_edit,
            dictionary::dictionary_reveal,
        ])
        .plugin(hotkeys::plugin(tx.clone()))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            updater.attach(app.handle());
            let cancel_app = app.handle().clone();
            pill::attach(app.handle(), pill_rx, move |armed| {
                let app = cancel_app.clone();
                let _ =
                    cancel_app.run_on_main_thread(move || hotkeys::set_cancel_armed(&app, armed));
            });
            tray::install(
                app.handle(),
                tray::Wiring {
                    inbox: setup_tx,
                    trigger: setup_trigger.clone(),
                    pill: tray_pill_tx,
                    history: setup_history,
                    ledger: setup_ledger,
                    status: setup_status,
                    sharing,
                    updater,
                },
            )?;
            paste::accessibility_trusted(true);
            if std::env::var_os("SEE_COMPUTER_OPEN_DICTIONARY").is_some() {
                dictionary::open(app.handle());
            }
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
