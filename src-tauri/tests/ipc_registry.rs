//! IPC registry safeguards.
//!
//! Catches regressions in three layers that unit tests do not see:
//! - 1) The `tauri::generate_handler!` list in `app/setup.rs` agrees with
//!      the source-of-truth list in `commands::COMMAND_NAMES`.
//! - 2) The `tauri_specta::collect_commands!` list in `app/setup.rs` agrees
//!      with the same source of truth (so the generated TS bindings match
//!      the actual Tauri-registered commands).
//! - 3) Every typed `tauri::State<'_, T>` parameter has a corresponding
//!      `.manage(T)` call — verified by building a real `tauri::test::mock_app()`,
//!      registering state via `manage_state`, registering commands via
//!      `generate_handler!`, and pinging each AppHandle-free command. Any
//!      "state not managed for field" surfaces as a test failure.
//!
//! Why these tests:
//! - The May 1 picker regression was a stale `invoke("ipc_list_prompts")`
//!   string in `picker.svelte` that no Rust unit test could see. The
//!   `scripts/lint-ipc.sh` script now blocks that class of bug at CI time.
//! - The `state not managed for field 'store'` regression on the same day
//!   was caused by adding a `tauri::State<'_, PromptStore>` parameter to a
//!   command without `.manage(ctx.prompts.clone())`. Test #3 below exercises
//!   the exact code path that hit and fails before merge.
//!
//! Platform note (Windows): the runtime smoke test (#3) requires the
//! `tauri/test` feature, which on Windows links against WebView2 at
//! executable load time. The CI runner's WebView2 component mismatches
//! at link load (`STATUS_ENTRYPOINT_NOT_FOUND`), so the entire test
//! binary fails to start. We gate this whole integration test on
//! non-Windows; the cross-check tests #1 and #2 are duplicated as unit
//! tests inside `commands::mod` so they still run on Windows CI.

#![cfg(not(target_os = "windows"))]

use prompt_player::app::context::AppContext;
use prompt_player::app::setup::manage_state;
use prompt_player::commands::COMMAND_NAMES;

// ---------------------------------------------------------------------------
// Cross-check #1: COMMAND_NAMES vs tauri::generate_handler! in setup.rs
// ---------------------------------------------------------------------------

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

/// The inline `.manage()` block in `run()` and the `manage_state` helper
/// list the same set of types. If you add a managed type to one but forget
/// the other, this test fails before merge.
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
            // Match .manage(...) and builder.manage(...) regardless of leading
            // whitespace. We extract the *managed type expression* and
            // normalize trivial differences that don't affect runtime
            // semantics (`ctx.clone()` and `ctx` both manage AppContext).
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
    // tauri-specta's collect_commands! lives in `generate_typescript_bindings`
    // — same file. Each entry has the form `crate::commands::module::name`,
    // so `.split("::").last()` still yields the bare name.
    let registered = extract_macro_paths(SETUP_RS, "collect_commands");
    let expected: Vec<String> = COMMAND_NAMES.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        registered, expected,
        "the `collect_commands!` (tauri-specta) list has drifted from \
         `commands::COMMAND_NAMES`. The generated TypeScript bindings \
         will be missing — or have stale — entries until this is fixed."
    );
}

// ---------------------------------------------------------------------------
// Smoke test #2: every command resolves and finds its managed state.
// ---------------------------------------------------------------------------
//
// `tauri::test::mock_builder()` returns a `Builder<MockRuntime>`, but our
// production commands are locked to `tauri::Wry` (because downstream
// telemetry / FireService / aptabase plugin paths are all Wry-typed). So we
// can't reuse `manage_state` + `generate_handler!` directly here.
//
// Workaround: we re-register the *no-AppHandle* commands against
// `MockRuntime` — those are the ones that depend purely on `tauri::State<>`
// and so exercise managed-state coverage. Together they touch all three
// state types (`Arc<AppState>`, `PromptStore`, `AppContext`), so any
// missing `.manage()` call surfaces here.
//
// The AppHandle-taking commands are not exercised at runtime by this test,
// but the cross-check tests above ensure they're registered, and the
// `lint-ipc.sh` script ensures the frontend can never call them with stale
// command-name strings.

use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Build a `tauri::test::App<MockRuntime>` with all production-equivalent
/// state managed and the no-AppHandle commands registered.
///
/// IMPORTANT: redirects the prompt library to a tempdir via the
/// `PROMPT_PLAYER_PROMPTS` env var BEFORE any IPC command that writes to
/// disk runs (`save_prompt`, `create_prompt`, `set_prompt_enabled`). Without
/// this, `cargo test` against the real binary would happily write
/// `smoke-created.pp.md` etc. into the developer's actual library at
/// `~/Library/Application Support/PromptPlayer/prompts/`.
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
            newline_mode: None,
            origin: Default::default(),
            body: "smoke".into(),
            source_path: None,
        }]);

    // Reuse the production `manage_state` function — guarantees this test
    // sees every `.manage()` call the real app uses. If a new managed type
    // gets added to production but a typed `tauri::State<>` consumer is
    // forgotten, this test catches it.
    manage_state(mock_builder(), ctx)
        .invoke_handler(tauri::generate_handler![
            // No-AppHandle subset of the production registry. Every
            // managed-state type is touched by at least one command here:
            // `get_armed` → Arc<AppState>, `list_prompts` → PromptStore,
            // `picker_search` → AppContext. The CRUD commands
            // (save/create/delete/set_enabled/set_pinned) now also take an
            // AppHandle (to reindex the matcher after a mutation), so they
            // join the AppHandle group and can't be registered against
            // MockRuntime — same as the armed/picker AppHandle commands.
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
    // Isolate disk writes: every `save_prompt`/`create_prompt` invocation in
    // this test would otherwise write a `.pp.md` file into the dev's real
    // library. Tempdir survives the whole test (held in scope until end);
    // dropped automatically with all files in it.
    let prompts_dir = tempfile::tempdir().expect("tempdir");
    let app = mock_app_with_state(prompts_dir.path());
    let webview = WebviewWindowBuilder::new(&app, "smoke", WebviewUrl::App("/".into()))
        .build()
        .expect("smoke webview");

    // For each command, send a mock IPC. The exact response shape varies by
    // command; we only assert "didn't return command-not-found / state-not-managed".
    // Tauri formats those as serde_json::Value with specific patterns; we
    // pattern-match conservatively (any 'state not managed' substring).
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
        // Distinguish two error layers:
        //   - Framework errors from Tauri itself (state-not-managed,
        //     command-not-found) come back as a JSON *string* — those are
        //     real bugs the smoke test must catch.
        //   - Application errors raised by our commands (e.g., our
        //     `IpcError` shape `{kind, message}`) come back as a JSON
        //     *object* — those are expected and not test failures.
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
