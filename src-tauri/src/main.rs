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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "prompt_player=info,info".into()),
        )
        .init();

    prompt_player::app::setup::run();
}
