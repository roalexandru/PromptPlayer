// Prompt Player entry point; wiring lives in `prompt_player::app::setup`.
// `#[tokio::main]` because plugins `tokio::spawn` from their setup hook.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tokio::main]
async fn main() {
    init_tracing();
    prompt_player::app::setup::run();
}

/// Three `tracing` sinks — stderr, a rolling per-user file, and Apple Unified
/// Logging. The appender guard is leaked so it outlives this function.
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
        let oslog_layer = tracing_oslog::OsLogger::new("com.roalexandru.promptplayer", "general");
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(file_layer)
            .with(oslog_layer)
            .init();
        eprintln!("Prompt Player logs: {}", log_dir.display());
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
