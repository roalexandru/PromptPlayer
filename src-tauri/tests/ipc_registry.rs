//! IPC registry safeguards: both macro lists match `COMMAND_NAMES`, and every
//! `tauri::State<'_, T>` has a matching `.manage`.
//!
//! The smoke test is gated off Windows, where `tauri/test` links WebView2 at
//! load; the cross-checks are duplicated in `commands::mod` to cover it.

#![cfg(not(target_os = "windows"))]

use prompt_player::app::context::AppContext;
use prompt_player::app::setup::manage_state;
use prompt_player::commands::COMMAND_NAMES;

// Cross-check #1: COMMAND_NAMES vs tauri::generate_handler! in setup.rs

const SETUP_RS: &str = include_str!("../src/app/setup.rs");

/// Extracts a comma-separated list of `path::path::name` items from a macro
/// invocation in `setup.rs`. Skips lines that aren't a path-like entry.
fn extract_macro_paths(src: &str, macro_name: &str) -> Vec<String> {
    let needle = format!("{}![", macro_name);
    let start = src.find(&needle).unwrap_or_else(|| {
        panic!("macro `{}` not found in setup.rs", macro_name);
    }) + needle.len();
    let rest = &src[start..];
    let end = rest.find(']').expect("unterminated macro invocation");
    let body = &rest[..end];

    body.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.starts_with("//"))
        .map(|s| s.split("::").last().unwrap().trim().to_string())
        .collect()
}

#[test]
fn generate_handler_list_matches_command_names() {
    let registered = extract_macro_paths(SETUP_RS, "tauri::generate_handler");
    let expected: Vec<String> = COMMAND_NAMES.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        registered, expected,
        "the `generate_handler!` list in setup.rs has drifted from \
         `commands::COMMAND_NAMES`. Add the new command name to \
         COMMAND_NAMES (or remove it from the macro) so all three \
         registries stay in lockstep."
    );
}

/// `run()`'s inline `.manage()` block and the `manage_state` helper list the
/// same types — adding to one and forgetting the other fails here.
#[test]
fn manage_state_helper_matches_inline_block_in_run() {
    fn extract_manage_chain(src: &str, marker: &str) -> Vec<String> {
        let chunk_start = src
            .find(marker)
            .unwrap_or_else(|| panic!("missing marker {marker}"));
        let chunk_end = (chunk_start + 1500).min(src.len());
        let chunk = &src[chunk_start..chunk_end];
        let mut managed = Vec::new();
        for line in chunk.lines() {
            let l = line.trim();
            // Extract the managed type expression, normalizing differences that
            // don't change runtime semantics (`ctx.clone()` vs `ctx`).
            for prefix in [".manage(", "builder.manage("] {
                if let Some(rest) = l.strip_prefix(prefix) {
                    if let Some(end) = rest.find(')') {
                        // For `ctx.state.clone()` the close-paren we want is
                        // the SECOND, not the first. Walk forward matching.
                        let mut depth = 1;
                        let bytes = rest.as_bytes();
                        let mut e = 0;
                        while e < bytes.len() {
                            match bytes[e] {
                                b'(' => depth += 1,
                                b')' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            e += 1;
                        }
                        let _ = end;
                        let raw = rest[..e].trim();
                        // Normalize: drop `.clone()` suffix.
                        let normalized = raw.strip_suffix(".clone()").unwrap_or(raw);
                        managed.push(normalized.to_string());
                        break;
                    }
                }
            }
            if l == "}" && !managed.is_empty() {
                break;
            }
        }
        managed
    }
    let inline = extract_manage_chain(SETUP_RS, "// Per-state managed handles");
    let helper = extract_manage_chain(SETUP_RS, "pub fn manage_state<R: tauri::Runtime>");
    assert_eq!(
        inline, helper,
        "the inline `.manage()` block in `run()` has drifted from \
         `manage_state()`. Add the new type to BOTH places (or remove \
         from both). The integration smoke test relies on the helper \
         matching production exactly."
    );
}

#[test]
fn collect_commands_list_matches_command_names() {
    // Entries here are `crate::commands::module::name`, so the same
    // `.split("::").last()` still yields the bare name.
    let registered = extract_macro_paths(SETUP_RS, "collect_commands");
    let expected: Vec<String> = COMMAND_NAMES.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        registered, expected,
        "the `collect_commands!` (tauri-specta) list has drifted from \
         `commands::COMMAND_NAMES`. The generated TypeScript bindings \
         will be missing — or have stale — entries until this is fixed."
    );
}

// Smoke test #2: every command resolves and finds its managed state. Production
// commands are `Wry`-typed, so we re-register the no-AppHandle subset here.

use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// A `MockRuntime` app with production state and the no-AppHandle commands.
/// Redirects the library to a tempdir before anything writes to disk.
fn mock_app_with_state(prompts_dir: &std::path::Path) -> tauri::App<tauri::test::MockRuntime> {
    // Set BEFORE constructing AppContext / running any save command. The env
    // var is process-wide; we keep it set for the whole test process.
    std::env::set_var("PROMPT_PLAYER_PROMPTS", prompts_dir);

    let ctx = AppContext::new();
    // Seed ONE prompt so list_prompts has something to return — gives us
    // a realistic non-empty payload to deserialize as a sanity check.
    ctx.prompts
        .replace_all(vec![prompt_player::prompts::Prompt {
            id: "smoke-test".into(),
            name: "Smoke test prompt".into(),
            description: String::new(),
            triggers: vec!["smoke".into()],
            commit_char: '>',
            priority: 0,
            typing_profile: Default::default(),
            typing_overrides: Default::default(),
            scope: None,
            filters: Vec::new(),
            hotkey: None,
            tags: Vec::new(),
            enabled: true,
            pinned: false,
            body: "smoke".into(),
            source_path: None,
        }]);

    // Reuse production's `manage_state` so this test sees every `.manage()`
    // call the real app makes.
    manage_state(mock_builder(), ctx)
        .invoke_handler(tauri::generate_handler![
            // No-AppHandle subset, between them touching all three managed
            // types. The CRUD commands take an AppHandle now, so they can't.
            prompt_player::commands::armed::get_armed,
            prompt_player::commands::prompts::list_prompts,
            prompt_player::commands::prompts::library_root,
            prompt_player::commands::picker::picker_search,
        ])
        .build(mock_context(noop_assets()))
        .expect("mock app build")
}

fn ping(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    payload: serde_json::Value,
) -> Result<tauri::ipc::InvokeResponseBody, serde_json::Value> {
    use http::header::HeaderMap;
    get_ipc_response(
        webview, // WebviewWindow: AsRef<Webview<MockRuntime>>
        InvokeRequest {
            cmd: cmd.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(payload),
            headers: HeaderMap::new(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
}

#[test]
fn smoke_test_no_apphandle_commands_resolve_with_managed_state() {
    // Isolate disk writes, or every `save_prompt` here lands in the dev's real
    // library. The tempdir is held in scope for the whole test.
    let prompts_dir = tempfile::tempdir().expect("tempdir");
    let app = mock_app_with_state(prompts_dir.path());
    let webview = WebviewWindowBuilder::new(&app, "smoke", WebviewUrl::App("/".into()))
        .build()
        .expect("smoke webview");

    // Send a mock IPC per command. Response shapes vary, so we only assert it
    // isn't command-not-found or state-not-managed.
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("get_armed", serde_json::json!({})),
        ("list_prompts", serde_json::json!({})),
        ("library_root", serde_json::json!({})),
        (
            "picker_search",
            serde_json::json!({ "q": "smoke", "limit": 5 }),
        ),
    ];

    for (cmd, payload) in cases {
        let res = ping(&webview, cmd, payload);
        // Tauri's own framework errors arrive as a JSON string and are real
        // bugs; our `IpcError` objects are expected and not failures.
        if let Err(e) = &res {
            // Application-level error: structured {kind,message} object.
            // Skip these — they're legitimate command output.
            if e.is_object() {
                continue;
            }
            // Framework error: typically a JSON string.
            let s = e.as_str().unwrap_or("").to_string();
            let lower = s.to_lowercase();
            assert!(
                !lower.contains("state not managed"),
                "command `{cmd}` failed with managed-state error: {s}\n\
                 Add a `.manage(...)` call in `app::setup::manage_state` for \
                 the missing type."
            );
            assert!(
                !lower.contains("command")
                    || (!lower.contains("not found")
                        && !lower.contains("unknown")
                        && !lower.contains("not allowed")),
                "command `{cmd}` is not registered or not permitted: {s}\n\
                 Add it to `tauri::generate_handler!` in `app::setup::run` \
                 (and to its capability manifest)."
            );
        }
    }
}
