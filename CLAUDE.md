# CLAUDE.md — Prompt Player

Guidance for working in this repository. Read this before making changes.

## What it is

**Prompt Player** is a **Tauri 2** desktop app — a *stealth keyboard utility for
live demos*, and a companion for coding agents (Claude Code and friends). You
store prompts, assign each a trigger word, and when you type `trigger>`
(trigger + a commit char, default `>`) in any text field the app silently
backspaces the trigger and continues typing the stored prompt at a
statistically realistic human cadence. It also offers a Command Palette
(fuzzy picker), a per-prompt hotkey system, a menu-bar/tray popover, an
ordered **setlist** with a next-cue hotkey, pause/speed transport controls,
import from agent prompt formats (`.claude/commands`, Cursor rules, …), and
read-only prompt **sources** fetched from public GitHub repos.

- **Backend:** Rust, in `src-tauri/` (a Cargo workspace).
- **Frontend:** Svelte 5 + TypeScript + Vite, in `src/`.
- **Bundle id:** `com.roalexandru.promptplayer` (locked; CI asserts it).
- **Targets:** macOS (arm64, min 11.0) and Windows (x64). Linux builds only as a
  dev convenience — several modules are `cfg`-gated to mac/win.

## Run / build / test

Package manager is **pnpm**. Common commands (from repo root):

| Task | Command |
|---|---|
| Dev app (hot reload) | `pnpm tauri dev` |
| Build frontend only | `pnpm build` (emits `dist/`) |
| Production bundle | `pnpm tauri build` |
| Frontend typecheck | `pnpm typecheck` (`svelte-check`) |
| Frontend unit tests | `pnpm test` (Vitest) |
| IPC lint | `bash scripts/lint-ipc.sh` |
| Rust tests | `cargo test --workspace --all-targets` |
| Rust lint | `cargo clippy --workspace --all-targets -- -D clippy::correctness` |
| Rust format check | `cargo fmt --all -- --check` |

**Gotcha:** `pnpm build` must run *before* any `cargo` build/test/clippy —
`tauri::generate_context!()` (in `app/setup.rs`) reads `frontendDist` (`../dist`)
at compile time and panics if `dist/` is missing. CI does this explicitly.

**Clippy is correctness-only** (`-D clippy::correctness`, not `-D warnings`)
while the legacy `cocoa`/`objc` → `objc2` migration is in flight (the old stack
emits ~140 deprecation warnings). Tighten once the legacy deps are gone.

## Workspace layout

Cargo workspace (`Cargo.toml` at root) with three member crates:
- `src-tauri/` — the app (bin `prompt-player`, lib `prompt_player`).
- `typing-engine-cli/` — standalone CLI over the typing engine; kept out of the
  bundle so the bundler/lipo step only fuses the main binary.
- `guest-helper/` — small helper binary.

Frontend `src/` is mounted through per-window HTML entry points at repo root:
`index.html` (library), `picker.html`, `tray-popup.html`, `about.html`.

## Backend module map (`src-tauri/src/`)

- `main.rs` / `lib.rs` — entry + module root. `lib.rs` is where new top-level
  modules are declared (some `cfg`-gated to mac/win).
- `app/` — application assembly:
  - `setup.rs` — **the assembly point** (like an Electron `main.js`): builds the
    `tauri::Builder`, registers plugins + managed state + the `invoke_handler!`
    command list, creates the tray icon, installs lifecycle hooks and shortcuts,
    spawns the update poller, and (debug only) regenerates the TS bindings.
  - `context.rs` — `AppContext`: the master `Clone` handle bundling every
    long-lived `Arc` (state, prompts, matcher, undo, focus, search, rdp,
    hotkeys, **power**).
  - `shortcuts.rs` — global shortcuts, `rebuild_prompt_hotkeys`,
    `refresh_tray_popup`. The chords live in `AppContext::globals` (an
    `RwLock<Globals>`) rather than being captured by the handler closure, so
    `reregister_globals` can rebind them from config without a relaunch.
  - `tray_flash.rs` — §2.7's red tray flash on kill. Derives the red icon from
    the baked-in tray asset at runtime, so it follows the per-platform icon
    choice automatically.
  - `fire.rs` — `FireService`, the typing pipeline (guard → gather context →
    expand → schedule → inject). `FireRequest` bundles the per-fire inputs.
  - `lifecycle.rs` — window close-to-hide / focus-loss handlers.
- `commands/` — **all IPC handlers**, one file per domain (`armed`, `config`,
  `power`, `prompts`, `picker`, `sources`, `tray`, `updater`, `library`,
  `shell`). `mod.rs` holds `COMMAND_NAMES` (the single source of truth — see
  IPC contract below).
- `state.rs` — `AppState`: runtime flags (`armed` + when it was armed,
  `playing`, the live `PlaybackControl`, panic-stroke ring, `commit_char`,
  setlist cursor, `hook_alive`). All in-memory.
- `config.rs` — **`promptplayer.yaml`** (§7.2): hotkeys, commit char, newline
  mode, text-field guard, picker display, auto-disarm, setlist, sources,
  `enabled-remote`, repo hints. `ConfigStore` on `AppContext` is the only
  reader/writer; `load_at`/`save_at` take an explicit path so tests are
  hermetic. A malformed file logs and falls back to defaults — it defines the
  global hotkeys, so it must never be able to brick them.
- `accessibility.rs` — focused-element inspection: is it safe to type here
  (§11 password/non-text guard) and what is selected (`$SELECTION`). Pure
  classification lives here; the platform reads are in
  `platform/macos/ax.rs` (AXUIElement) and `platform/windows/uia.rs`
  (UI Automation). **Fails open**: only `Secure` and `NotEditable` block a
  fire, because Chromium/Electron/terminal surfaces report generically.
- `usage.rs` — frecency history (`usage.json`) behind the picker's recents
  tier. Half-life 7 days; a completed fire is a use, a cancelled one is not.
- `sources.rs` — remote prompt sources: GitHub tarball fetch, `.pp.md`-only
  extraction with a path-traversal guard, per-source cache + manifest. Trust
  rules enforced on load: read-only, hotkeys dropped, disabled until the user
  enables them via `enabled-remote` in config.
- `repo.rs` — `$GIT_BRANCH` / `$REPO_NAME` / `$CWD` without shelling out
  (reads `.git/HEAD`, follows worktree `gitdir:` files).
- `prompts/agent_import.rs` — convert agent prompt files into `.pp.md`
  prompts. `$ARGUMENTS` becomes a `${1:arguments}` tab stop.
- `prompts/steps.rs` — multi-step sequences. A `<!-- pp:wait 25s -->` line
  splits a body into steps; `fire::play_sequence` types each, submits all but
  the last, and waits between. The wait is a **fixed duration** — the app
  cannot see the agent's output, and §14 rules out the injection tricks that
  would let it.
- `power/` — **"Keep Awake"** controller (`PowerManager`): inhibits display
  sleep / screensaver / idle system sleep. macOS = IOKit
  `PreventUserIdleDisplaySleep` assertion; Windows = `SetThreadExecutionState`
  on a dedicated owner thread; other = no-op. In-memory, resets each launch.
- `store/` — `PromptStore`: in-memory `Vec<Prompt>` + a `generation` counter;
  mutations write through to the `.pp.md` files on disk.
- `prompts/` — `Prompt` struct + `.pp.md` load/parse/watch (`library.rs`),
  placeholders, expressions (a QuickJS sandbox via `rquickjs`).
- `typer/` — human-cadence typing engine (profiles, schedule, typos,
  distributions, false starts). `PlaybackControl` carries cancel + pause +
  speed; `play_controlled` rebases its schedule origin on pause/speed change
  so absolute scheduling stays drift-free within each constant-speed stretch.
  `Key::ShiftEnter` and `ScheduleOptions::newline_mode` decide how an embedded
  newline is delivered (chat vs terminal agent).
- `matcher.rs` — trigger index + streaming keystroke matcher.
- `hook/` — global keyboard hook (`macos.rs` = native CGEventTap; `windows.rs` =
  `rdev`/SetWindowsHookEx). `inject/` — keystroke synthesis (Enigo + platform).
- `platform/` — all `unsafe` OS calls, split `macos/` vs `windows/` with
  **mirrored public APIs** so call sites are `cfg`-driven without branches.
  Windows-only: `menu.rs` (native tray menu), `tray_theme.rs` (icon light/dark
  swap). macOS-only: `monitor.rs` (`OutsideClickMonitor`), `nsworkspace.rs`.
- `picker/` — Command Palette window, focus store, fuzzy search.
- `telemetry.rs` — Aptabase events (whitelist enum). `tcc.rs` — macOS
  Accessibility (TCC) permission. `secure_input.rs`, `rdp.rs`, `scopes.rs`,
  `filters.rs`, `undo.rs`, `hotkey.rs`, `error.rs`.

## The two-implementation tray menu (important gotcha)

The tray "menu" you see on click has **two completely separate implementations**,
chosen at compile time:
- **macOS** → a Svelte webview popover: `src/windows/tray-popup.svelte` (an
  NSPanel styled to look native; JS-driven hover because WKWebView in a
  non-activating panel drops mouse-move events).
- **Windows** → a real native Win32 `HMENU` via `TrackPopupMenuEx`:
  `src-tauri/src/platform/windows/menu.rs` (webview popups have no reliable
  outside-click dismiss on Windows, so the OS owns the interaction).

**Adding/changing a tray item means editing BOTH** — the Svelte markup *and*
`menu.rs` (`run_menu` to render + an `ID_*` const + a `dispatch` arm) — usually
bridged by a shared `AppContext` field and an IPC command. The `armed` toggle
and the new `Keep Awake` toggle are the reference examples end-to-end.

## IPC contract (strict 3-place lockstep + codegen)

Every command is a Rust fn with `#[tauri::command] #[specta::specta]`. Its name
**must appear in all three** of:
1. `commands::COMMAND_NAMES` (`src-tauri/src/commands/mod.rs`) — source of truth.
2. `tauri::generate_handler![…]` (`src-tauri/src/app/setup.rs`).
3. `tauri_specta::collect_commands![…]` (`src-tauri/src/app/setup.rs`).

`commands::registry_tests` + `tests/ipc_registry.rs` fail the build on any drift
(they parse `setup.rs` and compare lists, order-sensitive), so keep the same
relative order in all three.

Codegen flow: specta emits `src/lib/ipc.gen.ts` (regenerated on every **debug**
launch by `setup.rs::generate_typescript_bindings` — *do not hand-edit*, or if
you must, keep it identical to what the generator would produce). The hand-written
façade `src/lib/ipc.ts` wraps it into the `ipc.*` object that Svelte calls;
`unwrap()` there turns `Result<T, IpcError>` into a throwing promise. `lint-ipc.sh`
forbids raw `invoke()` outside `$lib/ipc`.

## State & persistence

- Runtime flags (`AppState`) and Keep Awake (`PowerManager`) are **in-memory and
  reset every launch** — `armed` deliberately starts off (§10.1).
- **`promptplayer.yaml`** lives beside the prompts directory (i.e. in the
  *parent* of the library root — `config::config_root()`) and is read once at
  startup into `ConfigStore`. Everything except the global hotkeys is read
  per-use, so it applies live; hotkeys are registered with the OS once, so a
  rebind needs a relaunch (`save_config` returns a flag saying so).
  `tauri-plugin-store` is still not a dependency.
- **`usage.json`** (same directory) holds the frecency counters.
- **`sources/<owner-repo@ref>/`** (same directory) caches each remote source's
  `.pp.md` files plus a `.pp-source.json` manifest. The whole directory is
  replaced on refresh, which is why remote enablement lives in config instead.
- **Prompts persist as one `.pp.md` Markdown file each** under the OS app-data
  dir (macOS `~/Library/Application Support/PromptPlayer/prompts`, Windows
  `%APPDATA%\promptplayer\prompts`; override with `PROMPT_PLAYER_PROMPTS`).
  `set_enabled`/`set_pinned`/`save`/`delete` write straight through.
  `library.rs` hot-reloads on external file edits via a `notify` watcher.
- The one OS-persisted boolean is **autostart** (via `tauri-plugin-autostart`,
  LaunchAgent on mac / Run key on Windows) — not an app config file.
- App-data dir convention uses the `dirs` crate. Logs live under
  `dirs::data_local_dir()/PromptPlayer/logs/` (rolling `tracing-appender`).

## Platform-split conventions

- `#[cfg(target_os = "...")]` gating everywhere; `platform/{macos,windows}`
  expose the **same public API** so call sites don't branch.
- For functions that must compile on all targets, use the real impl +
  `#[cfg(not(...))]` no-op stub idiom (see `tcc.rs`, `power/mod.rs`).
- Target-gated Cargo deps in `src-tauri/Cargo.toml`: mac gets `core-*`,
  `cocoa`/`objc`, `objc2*`, `tracing-oslog`; win gets `rdev` + the `windows`
  crate (Win32 feature flags — add the needed feature when you call a new API,
  e.g. `Win32_System_Power` for `SetThreadExecutionState`).
- Frontend: `src/lib/platform.ts` exports synchronous `IS_MAC`/`IS_WIN`/etc.,
  resolved once at import — use it to make UI platform-conditional.

## Telemetry

Aptabase (`tauri-plugin-aptabase`), key `A-EU-9005405380`. Events are a
compile-time whitelist: the `TelemetryEvent` enum in `telemetry.rs` (each variant
also needs a `short_name()` arm). **Never logs prompt content / triggers /
expression source** — a test asserts no payload string exceeds 32 chars. Events
are dropped entirely when `PROMPT_PLAYER_E2E=1` (CI/e2e launches).

## Auto-update

`tauri-plugin-updater` pointed at the GitHub Releases `latest.json`
(`tauri.conf.json` → `plugins.updater`), verified with an embedded **minisign**
public key; Windows install mode is `passive`. `createUpdaterArtifacts: true`
emits the signed sidecars. A background poller (`setup.rs::spawn_update_poller`)
checks at startup +15s then every 6h and emits an `update-available`
`{ version, notes }` event; the tray popover / About window surface an
"Install update vX.Y.Z" row. Manual checks use the `updater_*` IPC commands.

## CI/CD & releases

- **`.github/workflows/ci.yml`** (push/PR to `main`): Ubuntu `smoke` gate
  (version-drift check, bundle-id, `lint-ipc`, `pnpm typecheck`, `pnpm test`) →
  `rust` matrix on macOS-arm64 + Windows-x64 (`fmt`, `clippy` correctness,
  `cargo test`). `pnpm build` runs first so `generate_context!` has `dist/`.
- **`.github/workflows/release.yml`** (on `v*` tag, or manual dispatch): `smoke`
  → `create-release` (draft) + `rust-tests` (macOS) → `release-platform` matrix
  (mac/win, in parallel) via `tauri-apps/tauri-action` which builds + signs +
  uploads `.dmg`/`.msi`/`.app.tar.gz`/`.sig`/`latest.json`, each leg gated by its
  own e2e script (`scripts/e2e-mac.sh` / `e2e-win.ps1`) → `publish-finalize`
  flips draft→published (stable → `--latest`; `-rc.x` etc → `--prerelease`) →
  `populate-changelog` appends auto-notes.
- **Signing secrets:** `TAURI_SIGNING_PRIVATE_KEY` / `…_PASSWORD` (updater
  minisign). Builds are currently **unsigned** re: Apple Developer ID / Windows
  EV cert (first-launch right-click→Open on mac; SmartScreen "Run anyway" on win).
- **Version must match** across `package.json`, `src-tauri/tauri.conf.json`, and
  root `Cargo.toml` (`version.workspace = true`) — both workflows fail otherwise,
  and release also checks the tag equals `package.json` version. **Bump all three
  together.**

## Companion features worth knowing about

- **Newline mode matters.** Terminals deliver Shift+Enter as a plain CR, so the
  chat-app default submits an agent prompt at its first blank line. Per-prompt
  `newline-mode:` overrides the library default; imported agent prompts are
  stamped `backslash-enter`.
- **The text-field guard fails open by design.** `accessibility::FieldKind`
  returns `Unknown` whenever the OS won't say, and `Unknown` proceeds. Adding
  roles to the "not editable" lists risks breaking exactly the Electron and
  terminal targets this app exists for.
- **Remote prompt ids are namespaced** `<source-id>/<stem>`, and locals are
  pushed into the store first so a trigger collision resolves in their favour.
- **Source updates are fetched but not applied.** The startup refresh updates
  each cache and emits `sources-updated`; `sources::pending_changes` diffs disk
  against the loaded set (stateless, so it can't drift) and
  `apply_source_updates` adopts it. Any source operation is refused while the
  app is armed or playing.
- **`git()` in expressions is triple-gated**: the config opts in, the prompt
  must be local, and a repo root must resolve. `shell()` is deliberately not
  implemented — see the note in `prompts::expressions`.
- **A pack can describe itself** via `promptplayer-pack.yaml` at the repo root
  (name, description, subdir, `min-app-version`). Read in a first pass over the
  archive, because it can redirect and gate the extraction that follows.
- **`play_controlled` may emit one key early** right after a pause or speed
  change; the next iteration rebases. That is deliberate (see the comment).

## Conventions & gotchas checklist

- Adding an IPC command → update `COMMAND_NAMES` + both macros in `setup.rs`
  (same order) + `ipc.ts` façade; `ipc.gen.ts` regenerates on debug launch.
- Adding a tray item → edit both `tray-popup.svelte` (mac) and `menu.rs` (win).
- Anything crossing IPC must avoid 64-bit integers: specta refuses to export
  `i64`/`u64`/`usize`. Use `u32`/`i32`, or a string for timestamps.
- `AppConfig` serializes **kebab-case** (it is the user-facing YAML), so the
  generated TS type has kebab keys — the frontend uses bracket access.
- `src/lib/ipc.gen.ts` regenerates on debug launch; a test asserts the
  committed file mentions every command, but never rewrites it. To refresh it
  without a display:
  `PP_REGEN_BINDINGS=1 cargo test -p prompt-player --lib regenerate_bindings`.
- Tests must never depend on `PROMPT_PLAYER_PROMPTS`: it is process-global and
  parallel tests raced each other's deleted temp dirs. `PromptStore::with_root`,
  `ConfigStore::with_path`, `UsageStore::with_path` and the `*_in` helpers in
  `sources` exist for that.
- Adding a managed `.manage()` type → edit BOTH the inline block in `run()` and
  `manage_state()` (a test enforces they match). Adding a field to `AppContext`
  is the low-friction alternative (it's already managed).
- Icons are baked into the binary via `include_bytes!` (runtime paths differ
  between `cargo run` and the packaged bundle).
- Bump version in all three manifests together.
