//! Process entry point and actor wiring.

use tauri::Manager;

mod cli;
mod config;
mod engine;
mod hotkeys;
mod mic;
mod paste;
mod pill;
mod recorder;
mod session;
mod share;
mod tray;
mod trigger;

fn main() {
    if let Some(cmd) = cli::parse(std::env::args().skip(1)) {
        std::process::exit(cli::run(cmd));
    }

    let mic = match std::env::var_os("SEE_COMPUTER_AUDIO_FILE") {
        Some(path) => mic::Source::Replay(path.into()),
        None => mic::Source::Default,
    };
    let (tx, rx) = std::sync::mpsc::channel::<session::Msg>();
    let (pill_tx, pill_rx) = std::sync::mpsc::channel::<pill::PillEvent>();
    let trigger = std::sync::Arc::new(std::sync::Mutex::new(config::Config::load().trigger));
    let controller = session::spawn(
        session::Wiring {
            mic,
            engine: engine::Loader::Models(engine::Models::default_root()),
            recorder: recorder::Recorder::screencapture(recorder::default_dir()),
            share: share::Share::LocalFile,
            paste: paste::Paste::system(),
            pill: pill_tx,
            trail: session::Trail::from_env(),
        },
        (tx.clone(), rx),
    );

    let setup_tx = tx.clone();
    let setup_watcher_tx = tx.clone();
    let setup_trigger = trigger.clone();
    let app = tauri::Builder::default()
        .plugin(hotkeys::plugin(tx.clone()))
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            pill::attach(app.handle(), pill_rx);
            trigger::set_app_handle(app.handle().clone());
            tray::install(app.handle(), setup_tx, setup_trigger.clone())?;
            hotkeys::register_defaults(app.handle());
            paste::accessibility_trusted(true);
            app.manage(trigger::spawn_watcher(setup_trigger, setup_watcher_tx));
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
