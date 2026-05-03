// Prompt Player — entry point. All app wiring lives in `prompt_player::app::setup`.
//
// We use `#[tokio::main]` because the Tauri plugins we depend on
// (notably `tauri-plugin-aptabase`) call `tokio::spawn` from their `setup()`
// hook, which requires a Tokio runtime to already exist on the calling
// thread. Tauri 2's default Builder doesn't install one, so we set it up
// ourselves.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tokio::main]
async fn main() {
    init_tracing();
    prompt_player::app::setup::run();
}

/// Wire `tracing` to multiple sinks so a runtime "trigger doesn't fire" report
/// always has somewhere to look:
///
/// 1. **stderr (fmt)** — useful for `cargo run` / launching from Terminal.
///    Release-mode .app bundles route stderr to /dev/null so this only helps devs.
/// 2. **rolling log file** — cross-platform; written to a stable per-user
///    location so users on Windows (no Console.app equivalent) and on Mac (who
///    don't know about Console.app) can attach the file to a bug report.
///    Mac:  `~/Library/Application Support/PromptPlayer/logs/prompt-player.log`
///    Win:  `%LOCALAPPDATA%\PromptPlayer\logs\prompt-player.log`
/// 3. **Apple Unified Logging (mac only)** — visible in Console.app filtered
///    by subsystem `com.roalexandru.promptplayer`, and via
///    `log stream --predicate 'subsystem == "com.roalexandru.promptplayer"'`.
///
/// The file appender's worker guard is leaked intentionally — we want it to
/// outlive `init_tracing()` and live for the entire process. Dropping it would
/// flush + close the file on shutdown, but for a long-running tray app the
/// process exit is the trigger, so leaking is fine.
fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "prompt_player=info,info".into());

    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);

    // Resolve the per-user log directory once. Falls back to system tmp on the
    // (very rare) failure case — better to log somewhere than silently nowhere.
    let log_dir = dirs::data_local_dir()
        .map(|d| d.join("PromptPlayer").join("logs"))
        .unwrap_or_else(|| std::env::temp_dir().join("prompt-player-logs"));
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, "prompt-player.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
    // Hold the guard for the lifetime of the process — see doc comment.
    Box::leak(Box::new(guard));
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_target(false);

    #[cfg(target_os = "macos")]
    {
        // `os_log` is keyed by subsystem; matching our bundle ID makes
        // filtering in Console.app intuitive (just type the bundle ID).
        let oslog_layer =
            tracing_oslog::OsLogger::new("com.roalexandru.promptplayer", "general");
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(file_layer)
            .with(oslog_layer)
            .init();
        eprintln!("Prompt Player logs: {}", log_dir.display());
        return;
    }

    #[cfg(not(target_os = "macos"))]
    {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(file_layer)
            .init();
        eprintln!("Prompt Player logs: {}", log_dir.display());
    }
}
