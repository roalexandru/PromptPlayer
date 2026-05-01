# Prompt Player — Specification

**Status:** Draft v0.3 — research-informed rewrite
**Author:** Alex
**Last updated:** 2026-04-30

> **What changed from v0.2** (driven by the pattern research document):
> - Typing engine completely respecified — log-normal mixture model, three profiles, pre-computed schedule, hierarchical pauses. The previous Gaussian/uniform sketch is replaced.
> - File format switched from JSON to **Markdown + YAML frontmatter** (`.pp.md`), aligning with Cursor / Claude Code / Continue / Copilot conventions. Audience already lives in this format.
> - Placeholder syntax: VS Code-style (`$1`, `${1:default}`, `${1|a,b,c|}`, `$CLIPBOARD`, `${var/regex/repl/flags}`) instead of inventing one. TypeScript expressions move to `${{ expr }}` to disambiguate.
> - **Form fields removed** — modal popups during expansion are a flow-killer for live demos. Variables resolve at script-load time or via clipboard/selection/expressions instead.
> - Picker is **hidden from screen capture** by default (`SetWindowDisplayAffinity` / `NSWindow.sharingType`).
> - Modifier-on-Enter for picker action variants (Enter = type, Shift+Enter = fast, Alt+Enter = paste, Cmd+Enter = run).
> - **Kill-switch global hotkey** added (immediate abort of in-progress typing).
> - RDP scenario gains an **optional Windows-side helper daemon** for pathological cases.
> - Layered/composable scopes with explicit `priority:` (vs Espanso's single-active-config rule).
> - Variable evaluation is **lazy** — only referenced vars run.
> - Promotes `{{cursor}}` (now `$0`) and multiple trigger aliases to v1.
> - Adds backspace-undo for misfired expansions.
> - Architecture: stable bundle ID across releases (avoids macOS Accessibility re-approval loop).

---

## 1. Purpose

A stealth keyboard utility for live demos. The presenter appears to type prompts in real-time; in reality, the app intercepts a trigger and continues typing the rest of a pre-stored prompt with human-like cadence. Targets native Windows and macOS apps, plus all browsers.

**Non-goals:**
- Not a general-purpose text expander (Espanso, Text Blaze, TextExpander cover that).
- Not a clipboard manager.
- Not a recording/automation tool (asciinema, Storylane cover that).
- Not a web-form-filling tool (Text Blaze Autopilot covers that).

**Two ways to fire a prompt:**
1. **Stealth trigger** (primary, §2) — type the first word + commit char `>`, app continues invisibly. For live demos.
2. **Picker** (§5) — hotkey opens a small floating window, user picks one, it types into the previously-focused app. For prep, rehearsal, fallback during demos.

**Positioning:** demo-magic is for terminal demos. Storylane/Walnut are for async product tours. Prompt Player is for **live demos of GUI apps and AI products** — the white space between those.

---

## 2. Core trigger model

The trigger is the **first word(s) of each stored prompt**, committed with `>` (configurable).

### 2.1 Match flow

1. User types the first word of a prompt naturally (e.g., `Build`).
2. User types `>`.
3. App detects: word(s) before `>` match a stored prompt's trigger.
4. App suppresses the `>` (it never reaches the focused application).
5. App pauses ~1.5 s (deliberate "I'm thinking" beat — see §3).
6. App continues typing the remainder of the prompt with human cadence.

**Example:**
- Stored prompt: `Build me a React component that displays a list of users with avatars and online status.`
- User types: `Build>`
- Audience sees: `Build` appear → ~1.5s pause → ` me a React component...` flowing in naturally with bursts, micro-hesitations, and the occasional typo+correction.

### 2.2 Match rules

- **Trigger word(s)** = contiguous run of non-whitespace chars (or multi-word sequence) immediately preceding the commit char.
- Match is **case-insensitive**, with **case propagation** (Espanso's term): typed prefix is preserved verbatim. `Build>` → `Build me a...`; `BUILD>` → `BUILD me a...`. The matcher stores prompts canonicalized; rendering applies the user's case.
- **Multi-word triggers supported.** Greedy longest-match. `Build me>` matches a 2-word trigger before falling back to a `Build` 1-word trigger. Multi-word match resets if the user pauses >2s between words.
- **Multiple aliases per prompt** (Espanso pattern): one prompt can be fired by any of several triggers. Useful for spoken-out-loud demo flow when you don't remember exactly how you stored it.
- **Uniqueness enforced** at edit time: no two prompts can share an exact trigger sequence (case-insensitive).
- **Filtering by app context** (§4): the same trigger can resolve to different prompts depending on the foreground app.

### 2.3 Commit character

- **Default `>`** but configurable globally and per-prompt.
- Alternatives: `»`, `→`, `;;`, `\`, custom.
- The commit char is suppressed (never reaches the focused app) on match. On no match, it passes through normally.

### 2.4 Escape hatches

- `\>` types a literal `>` even when armed. For shell redirects, markdown blockquotes, code comparisons in JSX/TS.
- **Backspace-undo:** within 2 seconds of a fired expansion, pressing Backspace repeatedly (or `Cmd/Ctrl+Z`) reverses the expansion and restores the original trigger word. Espanso ships this; it's critical demo recovery.

### 2.5 Failed match

If the word(s) before `>` don't match any prompt, `>` passes through normally. Imperceptible to user. The match-check is a hash lookup (sub-millisecond).

### 2.6 Cancellation safety

Three keystrokes within **600 ms** during the pre-typing pause or during playback cancel the expansion. Any key down counts (letters, digits, punctuation, arrows, Esc, Tab, function keys, Backspace) **except** lone modifier presses (Shift / Cmd / Ctrl / Opt by themselves). The keystrokes themselves are passed through to the focused app. Slower deliberate typing into another window does not abort, and a single twitchy keystroke is not enough (avoids accidental aborts).

### 2.7 Kill-switch hotkey

Global hotkey (default `Cmd/Ctrl+Shift+Esc`) **immediately aborts any in-progress typing**, regardless of armed state. Single most important safety mechanism for live demos. The kill-switch:
- Stops the typing thread.
- Releases all modifier keys (defensive).
- Briefly flashes the tray icon red.
- Logs a `prompt_killed` telemetry event.

---

## 3. Human typing engine

This is the entire product. Get it right. The literature gives us the answer; nobody else has implemented it.

### 3.1 Cadence model

**Inter-Key Interval (IKI) distribution** is a **mixture of two log-normals** — the empirically validated model from keystroke biometrics literature (Aalto 2018, Sequeira 2021, Roeser 2021). Not Gaussian, not uniform.

```
85%: LogNormal(μ=4.95, σ=0.35)   median ~140 ms   "fluent"
15%: LogNormal(μ=6.20, σ=0.50)   median ~490 ms   "micro-hesitation"
Clamp to [60 ms, 3000 ms]
```

**Hierarchical pauses** added on top of base IKI:

| Pause type | Distribution | Median |
|---|---|---|
| Word boundary | `+LogNormal(μ=5.7, σ=0.4)` | ~300 ms |
| Sentence boundary (`. ! ?` + space) | `+LogNormal(μ=7.0, σ=0.5)` | ~1.1 s |
| Paragraph boundary (`\n\n`) | `Normal(2500, 800)` ms | ~2.5 s |
| Pre-typing (after `>` suppressed) | `Normal(1500, 400)` ms | ~1.5 s |
| Pre-submit (before final Enter) | `Normal(1800, 600)` ms | ~1.8 s |

The **pre-submit pause** is the single most realism-defining touch. Audiences subconsciously expect a beat before "send." Most implementations skip it and feel mechanical as a result.

**Burst mode** (muscle-memory phrases): every 6–14 words, drop into `LogNormal(μ=4.7, σ=0.25)` for 8–20 chars. Simulates the "this phrase I've typed a hundred times" rhythm. Single most realistic touch on stage.

**Anti-pattern jitter:** add ±2–4 ms uniform noise to all scheduled times. Removes the "everything is a multiple of 16ms" tell that bot-detection classifiers (and observant audiences) catch. Costs nothing.

### 3.2 Typo model

- **Rate:** ~1 typo per 90 characters. **Lower than literature mean (1 per ~50)** — overdoing typos is the #1 implementation tell.
- **Skip typos** for prompts <30 chars, within trigger word, and within the first 5 chars of any expansion.
- **Type distribution:** 80% adjacent-QWERTY substitution, 15% transposition, 5% omission. Layout-aware (QWERTY default; AZERTY/Colemak configurable).
- **Detection latency:** 1–3 chars after the typo (uniform random).
- **"I noticed" pause:** `Normal(350, 100)` ms before correction.
- **Correction:** backspace `(latency_chars + 1)` times, then retype.

### 3.3 Profiles (presets, not 47 knobs)

Three named profiles. Each maps to all parameters above. User picks a profile; advanced users can tune.

| Profile | WPM target | IKI median | Typo rate | Pause variance | Use case |
|---|---|---|---|---|---|
| **Sales Engineer** (default) | 65 | 140 ms | 1/90 | medium | Most demos |
| **Fast Presenter** | 85 | 100 ms | 1/150 | low | Time-pressed talks, webinars |
| **Thoughtful CEO** | 45 | 220 ms | 1/120 | high (more re-reads) | Strategic prompts, exec demos |

**Per-prompt overrides** in YAML frontmatter (§7) let any prompt override profile parameters.

### 3.4 Pre-computed schedule

When a prompt fires, the engine **pre-computes the entire keystroke schedule before sending the first key**. Stolen from Duey.ai's pattern; nobody else does it.

Why: main-thread jitter (GC pauses, scheduling, OS interrupts) skews per-key timing if scheduled live. Pre-computing produces a list of `{key, absolute_time_ms}` tuples; the typer thread sleeps to each absolute time. Drift stays bounded; profile statistics actually match.

```rust
struct ScheduledKey {
    key: Key,             // char or special
    absolute_time_ms: u64, // since fire time
    is_correction: bool,  // typo correction backspace/retype
    is_burst: bool,       // muscle-memory bigram
}

fn schedule(text: &str, profile: &Profile, rng: &mut Rng) -> Vec<ScheduledKey> { ... }
```

### 3.5 Interruption

- **Kill-switch hotkey** (§2.7): immediate abort.
- **3 user keystrokes** during pre-typing pause or playback: cancel; keystrokes pass through.
- **Tray click → "Stop"**: same as kill-switch.
- Cancellation is **silent** (no popup, no notification). Demo-safe.

### 3.6 Bigram-aware delays (deferred)

Same-finger bigrams take 2–3× longer than alternating-hand; common bigrams (`th`, `he`, `in`) are 30–50% faster. **Skip in v1** — invisible at projector resolution. Revisit only if optimizing for keystroke-rhythm classifiers (which is anti-CAPTCHA territory, not demo territory).

---

## 4. Scopes (per-app prompt variants)

The single biggest demo enabler from the research. Same trigger, different prompt depending on foreground app.

### 4.1 Why this matters

The mental model "one trigger → one prompt" is wrong. The actual model is "one trigger → one *intent* → multiple *realizations* depending on context."

`intro>` conceptually means "introduce the demo." The introduction differs by surface:
- Cursor chat (technical, code-focused)
- A workflow-automation IDE's prompt field (domain-specific vocabulary, governance-focused)
- Slack DM (casual, shorthand)
- Jira comment (formal, structured)

Without scopes, you'd need 4 triggers (`introCursor`, `introIDE`, etc.) — defeating the natural-feeling first-word trigger.

### 4.2 Scope resolution

Each prompt declares optional scope filters. At trigger time, the matcher:
1. Captures foreground app metadata (bundle ID on Mac, executable + window title on Windows).
2. Filters candidate prompts to those whose scope matches.
3. Among matches, picks the one with highest `priority`.
4. Ties resolve to the most specific filter (more constraints = more specific).

```yaml
# In prompt YAML frontmatter
scope:
  app:
    - "com.todesktop.230313mzl4w4u92"     # Cursor bundle ID
    - "com.cursor.cursor"                    # alternate
  window-title-regex: ".*chat.*"
priority: 100   # higher wins on ties
```

Filters: `app` (bundle ID / executable / regex), `window-title-regex`, `url-regex` (browser tab when foreground is a browser), `time-of-day` (rarely useful but cheap to add).

### 4.3 Espanso comparison

Espanso supports per-app config (`filter_class`, `filter_title`, `filter_exec`) but **only one config can be active at a time** (longstanding issue #1077, open since 2022). Prompt Player uses **layered scopes with explicit priority** instead — multiple scoped prompt sets can be active simultaneously, conflicts resolve by priority + specificity.

---

## 5. Picker

Floating window, clipboard-history-style. For non-stealth use, rehearsal, and demo-recovery when you blank on the trigger.

### 5.1 Invocation

- Default hotkey: `Cmd/Ctrl+Shift+V` (mirrors clipboard-history convention).
- Works regardless of armed state.
- Captures foreground window handle on open.

### 5.2 UX

**Two-pane layout** (Raycast pattern): list on left, full preview on right.

- Top of list: recently-used prompts (last 50, ephemeral history).
- Below: pinned/labeled prompts (persistent collections — Paste's pinboard model).
- Search box: fuzzy by default via `nucleo-matcher` (Helix's matcher; Rust crate; produces highlight spans).
- Search index: prompt name, description, body, tags, target app.
- Number keys 1–9: select top items.
- Arrow keys: navigate.
- Esc: dismiss.

### 5.3 Modifier-on-Enter

| Modifier | Action |
|---|---|
| Enter | Type at human cadence (default) |
| Shift+Enter | Type fast (skip cadence — for already-demoed prompts) |
| Alt+Enter | Paste via clipboard (instant, breaks illusion) |
| Cmd+Enter | Run (for prompts that are slash-commands or tool calls — type and submit) |

### 5.4 Hidden from screen capture

**Default-on:** picker window is invisible to screen recording and broadcasts.

- Windows: `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`.
- macOS: `NSWindow.sharingType = .none` + `setSharingType(.none)`.

Toggle in settings ("Show during screen sharing") for rehearsal mode. **Critical for stealth demos** — without this, the picker pops up on the projector mid-demo.

Paste 6.0 added this exact toggle for the same reason.

### 5.5 Focus restoration

- Capture foreground app handle on picker open.
- On select: hide picker, activate previous app, wait ~150 ms, deliver prompt.
- Mac: `NSWorkspace.frontmostApplication` + Accessibility-API activation.
- Win: `GetForegroundWindow()` + `SetForegroundWindow()` + brief `AttachThreadInput` workaround for focus-stealing prevention.

### 5.6 Filter chain (Pastebot pattern)

Composable per-prompt transformations applied at fire time. Defined in YAML; chainable.

```yaml
filters:
  - lowercase-first-word
  - strip-thinking-blocks
  - inject-typos: { rate: 0.02 }
```

Built-ins: `lowercase`, `uppercase`, `capitalize`, `trim`, `strip-thinking-blocks`, `markdown-to-plain`, `inject-typos`, `regex-replace`. Custom filters via TypeScript expression.

---

## 6. Variables and expressions

### 6.1 Three layers

1. **VS Code-style placeholders** (the dominant mini-language in dev tooling): `$1`, `${1:default}`, `${1|a,b,c|}`, `$CLIPBOARD`, `$SELECTION`, `${var/regex/repl/flags}`, `$0` for cursor.
2. **Built-in context variables**: `$CLIPBOARD`, `$SELECTION`, `$DATE`, `$TIME`, `$DATETIME`, `$UUID`, `$APP_NAME`, `$APP_BUNDLE`, `$WINDOW_TITLE`, `$USER`, `$MACHINE`, `$RANDOM`.
3. **TypeScript expressions** (`${{ expr }}`, double-brace to disambiguate from VS Code single-brace) — sandboxed, for the small minority of advanced cases.

### 6.2 VS Code placeholder syntax

| Syntax | Meaning |
|---|---|
| `$1`, `$2` | Tab stops (Tab key advances cursor) |
| `$0` | Final cursor position |
| `${1:default}` | Tab stop with default value |
| `${1\|a,b,c\|}` | **Choice picker** (dropdown shown at fire time, but only ONE compact dropdown — see §6.4) |
| `$CLIPBOARD` | Current clipboard |
| `$SELECTION` | Selected text in foreground app |
| `${var/regex/repl/flags}` | Transformation |
| `/upcase`, `/downcase`, `/capitalize`, `/camelcase`, `/pascalcase`, `/kebabcase` | Case modifiers |

Example: `${SELECTION/(.*)/${1:/pascalcase}/g}` turns selected `user-profile` into `UserProfile`.

### 6.3 TypeScript expressions

Sandboxed via **`boa_engine`** (pure Rust, no native deps, ~5 MB) or **QuickJS** (smaller, faster, slightly harder to embed). Default to `boa_engine`.

```markdown
The repo is currently on commit ${{ git("rev-parse --short HEAD") }}.
${{ now.toISOString() }}: starting demo.
```

**Sandbox guarantees:**
- No filesystem access (except via explicit `git()` / `shell()` helpers, which are off by default).
- No network.
- 100 ms execution timeout.
- 10 MB memory cap.
- Frozen built-in variable surface: `now`, `today`, `clipboard`, `selection`, `app`, `env`, `random`, `random_choice([...])`, `format_date(d, fmt)`, `ago(d)`.

**Lazy evaluation:** expressions evaluate only when their slot is reached during typing. Skip-fast cancellation aborts pending evaluations. Espanso's bug — global vars evaluating on every match — is avoided structurally.

### 6.4 Why no modal forms

Text Blaze's `{formtext}`, Espanso's `[[field]]`, Beeftext's `#{input:}`, TextExpander's `%filltext%` all show a **modal popup mid-expansion**. **This is a flow-killer for live demos.** The audience sees a UI dialog appear from your invisible utility — instant immersion break.

**Replacement strategies:**
- **Choice placeholders** (`${1|a,b,c|}`) resolve via the **picker UI itself** before the picker dismisses, not a separate dialog.
- **Tab-stop placeholders** (`$1`, `$2`) leave the cursor at each stop after typing finishes; user fills in live (this is the natural "scaffold + live detail" demo pattern).
- **TypeScript expressions** for everything else.

---

## 7. File format

`.pp.md` — Markdown body + YAML frontmatter. Aligns with Cursor (`.cursor/rules/*.mdc`), Claude Code (`.claude/commands/*.md`), Continue (`*.prompt.md`), Copilot (`*.instructions.md`). Audience already lives in this format.

### 7.1 Single-prompt file

```markdown
---
name: refactor-to-async
description: Refactor a sync function to async/await
triggers: [refactor, refac, rfc]   # multi-alias
commit-char: ">"
priority: 100
typing-profile: sales-engineer      # or: fast-presenter, thoughtful-ceo, custom
typing-overrides:
  iki-median-ms: 130
  typo-rate: 0.005
scope:
  app:
    - com.todesktop.230313mzl4w4u92   # Cursor
filters:
  - strip-thinking-blocks
hotkey: cmd+shift+1
tags: [refactor, async, typescript]
---

Refactor this code to use async/await instead of `.then()` chains.

Selected code:
$SELECTION

Style preferences: ${1|aggressive,conservative|}
Filename context: ${TM_FILENAME}

Return only the refactored code, no commentary.

$0
```

### 7.2 Library-level config

```yaml
# ~/.config/promptplayer/promptplayer.yaml
profile-default: sales-engineer
commit-char-default: ">"
hotkey-arm: cmd+shift+p
hotkey-picker: cmd+shift+v
hotkey-kill: cmd+shift+escape

prompts:
  - uses: ./prompts/work/                      # all .pp.md in dir
  - uses: ./prompts/cursor-demos.pp.md
  - uses: github:org/team-prompts@main       # Continue-style hub ref
  - uses: file://~/team-prompts/onboarding.pp.md
```

### 7.3 Why Markdown + YAML

- **Engineering audience already authors prompts in this format** (Cursor, Claude Code, Copilot all converged on it 2024–2025).
- Reviewable in PRs.
- Versionable in git.
- Composable via `uses:` references.
- Plain-text editable in any editor; no proprietary chip UI.
- Markdown body renders nicely in editors with frontmatter folded.

### 7.4 Storage layout

```
~/.config/promptplayer/                  # Mac: ~/Library/Application Support/PromptPlayer
├── promptplayer.yaml                    # Library-level config
├── prompts/                             # User prompts
│   ├── default/
│   │   ├── intro.pp.md
│   │   └── refactor.pp.md
│   └── work/
│       └── live-demo.pp.md
├── filters/                             # Custom filter scripts
│   └── strip-internal-tags.ts
└── state.db                             # SQLite: usage history, encrypted
```

---

## 8. Architecture

### 8.1 Stack

- **Shell:** Tauri 2.x.
- **Backend:** Rust.
- **Frontend:** Svelte/React + TypeScript (config UI; runs in tray).
- **Storage:** Markdown files for prompts; SQLite (SQLCipher) for usage history; YAML for config.

### 8.2 Architectural reference: Espanso + Beeftext

Espanso is the closest open-source reference. Its module split (per-platform `Source`/`Injector`/`AppInfoProvider`/`Clipboard`) is the right factoring. Beeftext's `InputManager` is the reference for Windows keystroke injection.

**Five places to diverge from Espanso** (all map to longstanding open issues):

| Espanso pain | Prompt Player answer |
|---|---|
| Single active config (#1077) | Layered/composable scopes with `priority:` |
| Fuzzy search missing (#1163) | Ship `nucleo-matcher` from day one |
| Global vars run on every match (#270) | **Lazy evaluation** — only referenced vars run |
| macOS Accessibility re-approval on every release (#2562) | **Stable bundle ID** across releases; built-in TCC reset utility |
| `inject_delay`/`key_delay` chaos | Single logical timing model (profiles), platform overrides hidden |

### 8.3 Key crates

- `enigo` or platform-native — keystroke synthesis.
- `rdev` or platform-native hooks — keyboard listening with suppression.
- `tauri-plugin-global-shortcut` — arm/disarm, picker, kill-switch hotkeys.
- `tauri-plugin-autostart` — launch on login.
- `tauri-plugin-updater` — auto-update from GitHub Releases.
- `boa_engine` — sandboxed JS/TS evaluator.
- `nucleo-matcher` — fuzzy search.
- `aptabase` — telemetry.
- `serde_yaml`, `pulldown-cmark` — config and prompt parsing.
- `rusqlite` + `sqlcipher` — encrypted local state.

### 8.4 Module layout

```
src-tauri/src/
├── main.rs               // Tauri setup, tray, lifecycle
├── hook/
│   ├── mod.rs            // Cross-platform listener trait
│   ├── windows.rs        // SetWindowsHookEx (WH_KEYBOARD_LL)
│   └── macos.rs          // CGEventTap on dedicated CFRunLoop thread
├── inject/
│   ├── mod.rs            // Cross-platform injector trait
│   ├── windows.rs        // SendInput w/ scan-code preservation
│   └── macos.rs          // CGEventCreateKeyboardEvent
├── matcher.rs            // Multi-word trigger detection, scope resolution
├── typer/
│   ├── mod.rs            // Schedule pre-computation, playback
│   ├── distributions.rs  // Log-normal mixture, hierarchical pauses
│   ├── typos.rs          // Adjacent-key model, correction
│   └── profiles.rs       // Sales-engineer, fast-presenter, thoughtful-ceo
├── prompts/
│   ├── mod.rs
│   ├── parser.rs         // YAML frontmatter + Markdown body
│   ├── placeholders.rs   // VS Code-style $1, ${1|a,b|}, ${var/.../...}
│   └── expressions.rs    // boa_engine TS sandbox
├── picker/
│   ├── window.rs         // Tauri window, screen-capture exclusion
│   ├── search.rs         // nucleo-matcher index
│   └── focus.rs          // Save/restore foreground window
├── scopes.rs             // App detection, scope priority resolution
├── filters.rs            // Filter chain (strip-thinking, lowercase, etc.)
├── rdp.rs                // RDP detection, optional guest-helper IPC
├── telemetry.rs          // Aptabase (no prompt content logged)
├── undo.rs               // Backspace-undo for misfired expansions
├── state.rs              // Armed/disarmed, current playback
└── ipc.rs                // Tauri commands for frontend
```

### 8.5 Threading model

- **Main thread:** Tauri event loop + webview.
- **Hook thread (Mac):** dedicated `CFRunLoop` thread for `CGEventTap`. macOS requires this; the tap callback must respond <1s or macOS auto-disables it.
- **Hook thread (Windows):** dedicated thread with message pump for `SetWindowsHookEx`.
- **Typer thread:** spawned per playback. Owns the scheduled keystroke list. Cancellable via atomic flag. Releases all modifiers on abort.
- **Match thread:** receives keystrokes from hook via channel; resolves scope + trigger; sub-millisecond hash lookup; pushes to typer.

### 8.6 Save/restore foreground window

Required for picker focus restoration and RDP detection.

- macOS: `NSWorkspace.frontmostApplication` for bundle ID; `AXUIElementCopyAttributeValue` for window title.
- Windows: `GetForegroundWindow()` + `GetWindowTextW()` + `GetWindowThreadProcessId()` + `QueryFullProcessImageNameW()` for executable.

---

## 9. Platform-specific concerns

### 9.1 macOS

**Permissions required:**
- **Accessibility** — for keystroke synthesis (`enigo` / `CGEventPost`) and foreground-app querying.
- **Input Monitoring** — for `CGEventTap` to receive global keystrokes.
- **Screen Recording** is **NOT** required (and we never request it).

**First-run UX:** detect missing permissions, show a modal with deep-links to the relevant System Settings panes. Never silently fail — the #1 reason these tools feel broken.

**Stable bundle ID** across all releases (`com.roalexandru.promptplayer` or similar). Espanso's biggest UX disaster is bumping the bundle ID per release, which invalidates Accessibility approval and makes users re-approve on every update. We avoid this entirely.

**TCC reset utility** built into the app for cases where permissions get stuck:
- Detect "approved but not working" via test-tap with timeout.
- Surface a one-click "Reset & Reapprove" that runs `tccutil reset Accessibility com.roalexandru.promptplayer` and walks the user back through approval.

**Secure Input detection:** when password fields are focused (1Password, Keychain prompts, Terminal `sudo`), macOS engages Secure Event Input which blocks `CGEventTap` from suppressing keystrokes. Detect via `IsSecureEventInputEnabled()` and:
- Disable trigger detection while active (passes everything through).
- Show tray icon "🔒 Secure Input Active" indicator.
- Log telemetry event (no content).

**Notarization:** required for distribution. Apple Developer ID + `notarytool` in CI. Without it, users hit Gatekeeper warnings.

### 9.2 Windows

**Permissions:** none for standard user.

**Hook:** `SetWindowsHookEx(WH_KEYBOARD_LL, ...)`. Returning non-zero from callback suppresses the keystroke.

**Elevated apps:** standard hooks don't intercept keystrokes in apps running as Administrator. Workaround: ship a UI Access manifest variant (like AHK does with `AutoHotkey64_UIA.exe`). Defer to v2; document v1 limitation.

**Antivirus:** keyboard hooks + keystroke injection are heuristically flagged. Plan from day one:
- **EV code-signing certificate** (~$200/yr; one-time pain to acquire). Reduces Defender SmartScreen friction dramatically.
- **AV vendor outreach** before 1k installs (Defender, Symantec, BitDefender, Kaspersky). Beeftext has an 8-year history of false-positive triage; learn from their public "Beeftext is safe" wiki and pre-empt.

**Distribution:** `.msi` via Tauri bundler.

### 9.3 RDP scenario (Mac → Windows VM)

Genuinely supported. Two architectures, both shipped:

**Architecture A — Host-side typing (default, ~95% of cases):**
- App runs on Mac.
- Trigger detection listens to physical Mac keyboard via `CGEventTap`.
- Keystroke synthesis sends to focused Mac app (the RDP client window).
- RDP client forwards keys to Windows session naturally.
- **Detection:** foreground bundle ID matches a known RDP client (`com.microsoft.rdc.macos`, `com.parallels.desktop.console`, `com.vmware.fusion`, `com.citrix.receiver.icaclient`, etc.).
- **RDP-mode adjustments** when active:
  - Minimum inter-key delay floor: 30 ms (RDP clients drop bursts).
  - Speed multiplier: ×1.3 slower than configured profile.
  - Disable clipboard fallback (RDP clipboard sync is unreliable).
  - Backspace coalescing: send single events, not bursts.
- Recognized RDP-client list editable in settings.

**Architecture B — Guest-side helper daemon (optional, for pathological cases):**
- Tiny Windows daemon (~2 MB MSI) installed inside the Windows VM.
- Listens on local TCP port (default `127.0.0.1:9847`) or named pipe.
- Mac app sends `{prompt_text, schedule}` over the connection.
- Daemon types locally inside the VM.
- More reliable for: high-latency RDP sessions, complex Unicode, IME-heavy languages.
- Auth: shared secret in config file readable only by the user.
- **Off by default.** Surfaced when host-side typing fails (latency spike detected) with one-click install offer.

**Limitation:** if user RDPs *into* the Mac from elsewhere, behavior is untested and unsupported.

### 9.4 Cross-platform Unicode

`enigo`/`CGEventCreateKeyboardEvent`/`SendInput` all type via key events, which are layout-dependent. For non-ASCII chars in prompts:
- Default: type-by-character with Unicode keycode (`CGEventCreateKeyboardEvent` w/ `keyCode = 0` and `setUnicodeString`; Windows `KEYEVENTF_UNICODE` flag).
- Fallback: clipboard paste for runs of >5 non-ASCII chars (saves clipboard, sets, pastes, restores).
- Disabled in RDP mode (clipboard sync unreliable).

Tested explicitly for: emoji, accented Latin, CJK, Cyrillic, Arabic.

---

## 10. UI

### 10.1 Tray

Two states: armed (filled icon, accent color) / disarmed (outline). Click toggles. Right-click menu: Open library, Open picker, Settings, Quit, About.

App **starts disarmed every launch.** Never persists "armed" state across restarts.

### 10.2 Library window

- Tree view of prompts (folders match filesystem layout).
- Per-prompt editor: Markdown source on the left, **live cadence preview pane on the right**.
- The preview pane runs the typing engine into a sandboxed text area, showing exactly what an audience would see. Single most useful authoring feature; nobody else has it.
- Trigger uniqueness validation inline.
- Scope auto-detection helper: "Capture current foreground app" button to fill in `scope.app:`.
- Expression "Test" button: evaluates `${{ ... }}` blocks with current context.
- Import/export `.pp.md` files.

### 10.3 Settings

- Profile selection (Sales Engineer / Fast Presenter / Thoughtful CEO / Custom).
- Custom-profile sliders (advanced, behind disclosure).
- Hotkeys: arm/disarm, picker, kill-switch, panic-reset.
- Commit-char default.
- Auto-disarm timer (default off, suggested 30 min).
- "Show picker during screen sharing" toggle (default off — i.e., hide).
- RDP client list editor.
- Telemetry toggle (debug builds only — prod is always-on per Q5).
- Update channel: stable / beta.

### 10.4 Authoring shortcuts

- New prompt from clipboard: `Cmd/Ctrl+Shift+N` while picker open.
- Promote recent-history item to pinned: `Cmd/Ctrl+P`.
- Edit current prompt in default editor: `Cmd/Ctrl+E`.

---

## 11. Safety, recovery, and failure modes

| Risk | Mitigation |
|---|---|
| Fires during private message | Default disarmed; explicit arm; auto-disarm timer; Secure Input detection. |
| Fires in password field | Secure Input detection (Mac); password-field heuristic via Accessibility role. |
| Fires on wrong prompt mid-demo | Backspace-undo within 2s. |
| Typing runs away | Kill-switch hotkey (immediate); 3-keystroke cancel; tray "Stop". |
| Hook crashes target app | Hook callback is fire-and-forget into a channel; matcher on separate thread. |
| Audience notices typing artifact | Trigger word IS the first word — by design no visible artifact. Picker hidden from screen capture. |
| Prompt library on wrong machine | `.pp.md` files are portable; export/import via filesystem; optional git sync. |
| macOS permissions stuck | TCC reset utility built in. |
| Windows AV flags app | EV-signed; AV vendor outreach; `Beeftext is safe`-style wiki page. |
| RDP latency causes drops | RDP-mode timing + optional guest-side helper. |
| User in audience screen-records demo | Picker hidden by default; presenter can rehearse without it leaking. |
| Modifier keys stuck after kill | Kill-switch explicitly releases all modifiers. |

---

## 12. Telemetry (Aptabase)

Per Q5: minimum viable, no opt-out, debug vs prod separation, no prompt content logged.

**What we log:**
- `app_started` (version, OS, locale, profile-in-use).
- `prompt_fired` (mode: stealth/picker, char_count_bucket, has_expressions, target_app_kind: browser/native/rdp, scope_match: yes/no).
- `prompt_cancelled` (reason: user_keystrokes/esc/error/kill, completed_chars_pct).
- `prompt_killed` (kill-switch invoked).
- `prompt_undone` (backspace-undo within 2s).
- `picker_opened`, `picker_dismissed`, `picker_search_chars`.
- `arm_toggled`.
- `expression_error` (error_kind only, never source).
- `update_check`, `update_applied`.
- `secure_input_detected`.
- `rdp_detected`.

**What we never log:**
- Prompt content, trigger words, expression source.
- Clipboard, selection, window titles.
- Form field input.
- Anything reconstructible.

**Configuration:**
- Two Aptabase keys: debug build, prod build.
- Build-time selection via `cfg!(debug_assertions)`.
- Always on in prod.

---

## 13. Distribution

Mirrors VideoNarrator approach.

**Build artifacts:**
- macOS: `.dmg` universal binary (aarch64 + x86_64). Notarized.
- Windows: `.msi` x64. EV-signed.

**Auto-update:**
- `tauri-plugin-updater` against GitHub Releases.
- Updater public key in `tauri.conf.json`; private key in CI secrets.
- Releases signed with the Tauri updater key (separate from code-signing).
- Check on startup + every 6h. User notified; install on next restart (default) or immediate.

**Code signing (v1):**
- macOS: **Apple Developer ID + notarization** (required; Gatekeeper otherwise blocks). $99/yr.
- Windows: **EV code-signing certificate** (~$200/yr). Reduces SmartScreen friction.
- Both essential for distribution beyond a handful of personal machines.

**CI/CD:**
- GitHub Actions matrix: `macos-latest` (universal via lipo), `windows-latest` x64.
- Tag-triggered releases (`v*`).
- Reuse VideoNarrator's release workflow as starting template.

**Install footprint:**
- Mac: `/Applications/Prompt Player.app` + config in `~/Library/Application Support/PromptPlayer/`.
- Windows: `%LOCALAPPDATA%\Programs\PromptPlayer\` + config in `%APPDATA%\PromptPlayer\`.

**Distribution channels:**
- v1: GitHub Releases.
- v1.1: Homebrew cask (`brew install --cask promptplayer`).
- v1.1: Winget (`winget install promptplayer`).
- v2: MAS / Microsoft Store (only if friction warrants it; sandboxing makes Accessibility hooks harder).

---

## 14. What we deliberately omit

(From research: features that look attractive but actively hurt the live-demo use case.)

- **Modal form popups during expansion.** Flow-killer. Replaced by tab-stops, choice placeholders, expressions.
- **Cloud sync.** Local-first sidesteps SOC2/HIPAA. Optional git/gist export instead.
- **Custom formula DSL.** TypeScript expressions are enough. One scripting surface, not two.
- **Web automation** (`{click}`, Autopilot, `{urlload}`). Use Playwright for that.
- **Database/API integrations.** Out of scope.
- **Rich-text and image insertion.** Plain Unicode + Markdown source.
- **Interactive in-snippet UI** (buttons, toasts, mutable state).
- **Visual chip editor.** Engineering audience prefers Markdown source.
- **Synthetic `KeyboardEvent` for typing in browsers.** Not `isTrusted`; modern editors ignore. Reserve for hotkeys (Enter to submit) only.
- **`chrome.debugger`-based injection.** Yellow banner kills stealth.
- **Asciinema-style replay.** Defeats the point of demoing live AI.
- **Bigram-aware delays in v1.** Invisible at projector resolution; defer to v2.

---

## 15. Resolved decisions

1. **Q1 — Case sensitivity:** insensitive, with case propagation. ✅
2. **Q2 — Multi-word triggers:** supported, greedy longest-match, 2s pause resets, multi-alias. ✅
3. **Q3 — App-aware pause:** dropped. Manual tray arm/disarm + kill-switch. ✅
4. **Q4 — Variables:** VS Code placeholders + sandboxed TypeScript (`boa_engine`). ✅
5. **Q5 — Telemetry:** Aptabase, events only, no prompt content, debug/prod split. ✅
6. **Q6 — Distribution:** MSI + DMG, Tauri auto-updater, EV/Developer ID signing, GitHub Releases. ✅
7. **Q7 — Race condition during pause:** 3 keystrokes to cancel; kill-switch for hard abort. ✅
8. **Q8 — Trigger commit char:** configurable, default `>`. ✅
9. **Q9 — Theatrical pauses:** baked into "Thoughtful CEO" profile. ✅
10. **Q10 — `>` after non-trigger word:** passes through; no special handling. ✅
11. **NEW — File format:** Markdown + YAML frontmatter (`.pp.md`), aligned with Cursor/Claude Code/Continue. ✅
12. **NEW — Per-app scopes:** layered with `priority:`, multiple active configs. ✅
13. **NEW — Picker hidden from screen capture:** default-on. ✅
14. **NEW — Backspace-undo for misfired expansions:** within 2s. ✅
15. **NEW — Kill-switch hotkey:** `Cmd/Ctrl+Shift+Esc` default. ✅
16. **NEW — RDP guest-side helper daemon:** optional, off by default. ✅

---

## 16. Build phases

**Phase 0 — Spec lock.** ✅ (this document, v0.3)

**Phase 1 — Typing engine prototype.** Standalone Rust binary. Reads text from stdin, types into focused window with full log-normal mixture cadence, hierarchical pauses, typo+correction, three profiles. CLI flags for tuning. **The single most important phase** — get the feel right in isolation. Includes pre-computed schedule + RDP-mode flag.

**Phase 2 — Keyboard hook prototype (both platforms).** Two binaries (Win + Mac). Listen, buffer last word(s), detect `word>` and multi-word triggers, log to console. No suppression yet.

**Phase 3 — Suppression + multi-word matching.** Add commit-char suppression both platforms. Verify no leakage. Multi-word greedy match. Backspace-undo prototype.

**Phase 4 — Tauri integration.** Combine into Tauri app skeleton. Static prompt list. Tray icon, arm/disarm hotkey, kill-switch hotkey. Stable bundle ID locked.

**Phase 5 — Markdown library.** YAML frontmatter parser, VS Code placeholder syntax, file-watch for live reload. Library editor UI with cadence preview pane.

**Phase 6 — Picker.** Floating window, `nucleo-matcher` fuzzy search, screen-capture exclusion, modifier-on-Enter, focus restoration, two-tier history+pinned.

**Phase 7 — Scopes.** Per-app filter resolution, priority + specificity, scope auto-detect helper.

**Phase 8 — Expressions.** `boa_engine` TS sandbox, built-in variable surface, lazy evaluation, expression Test button in editor.

**Phase 9 — RDP host-mode.** Foreground RDP-client detection, RDP-mode timing adjustments, validation against Microsoft Remote Desktop / Parallels into Windows 11 VM.

**Phase 10 — Telemetry + updater.** Aptabase wiring (debug/prod split). Tauri updater configured against GitHub Releases. EV cert + notarization in CI.

**Phase 11 — Distribution.** GitHub Actions matrix build for `.dmg` (universal) + `.msi`. Release workflow tagged from VideoNarrator. AV-vendor outreach started.

**Phase 12 — RDP guest-helper (optional).** Tiny Windows daemon, IPC over local TCP, host-side fallback offer when latency detected.

**Phase 13 — Polish.** Permissions UX, TCC reset utility, Secure Input detection, panic-reset hotkey, README with install + RDP setup + AV-allowlist instructions.

**Phase 14 — Field test.** Real (low-stakes) demo. Iterate.

---

## 17. Out of scope for v1

- Linux support (deferred; Wayland is a nightmare per Espanso's experience).
- Multi-step prompts (type, wait for response, type follow-up). Maybe v2.
- Voice trigger.
- Mobile companion.
- Cloud sync of prompt library.
- Team sharing UI (use git for now).
- `chrome.debugger`-based browser injection.
- Bigram-aware typing delays.
- Form-field modal popups (deliberate; never).
- Web automation / Autopilot-style.
- Custom formula DSL.
- Code-signing-free distribution at v1 (defer-able means we ship signed from day one).
