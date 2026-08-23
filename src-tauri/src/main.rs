//! Process entry point and actor wiring.

mod cli;
mod engine;
mod hotkeys;
mod mic;
mod paste;
mod pill;
mod recorder;
mod session;
mod share;
mod tray;

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

    tauri::Builder::default()
        .plugin(hotkeys::plugin(tx.clone()))
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            pill::attach(app.handle(), pill_rx);
            tray::install(app.handle(), tx.clone())?;
            session::spawn(
                session::Wiring {
                    mic,
                    engine: engine::Loader::Models(engine::Models::default_root()),
                    recorder: recorder::Recorder::screencapture(recordings_dir()),
                    share: share::Share::LocalFile,
                    paste: paste::Paste::system(),
                    pill: pill_tx,
                    trail: session::Trail::from_env(),
                },
                (tx, rx),
            );
            hotkeys::register_defaults(app.handle())?;
            paste::accessibility_trusted(true);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("see.computer failed to start");
}

fn recordings_dir() -> std::path::PathBuf {
    dirs::video_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("see.computer")
}
