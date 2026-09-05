# Prompt Player

Stealth keyboard utility for live demos. Type the first word of a stored prompt + `>`,
and the app suppresses the `>` and continues typing the rest with statistically realistic
human cadence — log-normal mixture distribution, hierarchical pauses, occasional typos with
corrections, the works.

See [`prompt-player-spec.md`](./prompt-player-spec.md) for the full v0.3 specification.

## Status

Active build, following the 14-phase plan in `prompt-player-spec.md` §16.

Beyond the stealth-trigger core it now works as a **coding-agent companion**:

- **Import what you already wrote.** Pull in `.claude/commands`, Claude Code
  skills, Cursor rules, and Continue/Copilot prompt files from any project.
- **Type & send.** `Cmd/Ctrl+Enter` in the palette types the prompt and submits
  it. Line breaks adapt to the target, because terminals treat Shift+Enter as a
  plain Enter and would submit an agent prompt at its first blank line.
- **Repo context.** `$GIT_BRANCH`, `$REPO_NAME` and `$CWD` resolve from the
  checkout you are demoing from, with no `git` subprocess.
- **Setlist.** An ordered list of cues and one hotkey that fires the next one,
  for when recall fails on stage.
- **Pause, resume, re-speed** a playback mid-prompt without losing the rest.
- **Shared sources.** Point at a public GitHub repo of `.pp.md` files; they
  load read-only and stay disabled until you enable them.
- **Recents.** The palette's default order is frecency, not filesystem order.
- **Safety.** Refuses to type into password and non-text fields, and can
  auto-disable itself after a configurable idle period.

Cross-cutting settings live in `promptplayer.yaml` next to your prompts
(`§7.2`); the library window's **Companion** tab edits that same file.

## Quick start (dev)

```bash
pnpm install
pnpm tauri dev
```

First run on macOS will request **Accessibility** and **Input Monitoring** permissions.
Both are required (Screen Recording is *not* required and never requested).

## Architecture

- **Backend:** Rust + Tauri 2.x.
- **Frontend:** Svelte 5 (config UI; runs from tray).
- **Storage:** Markdown files for prompts; SQLite (SQLCipher) for usage history; YAML for config.

See `prompt-player-spec.md` §8 for the full architecture.

## Distribution

Installable artifacts will land in GitHub Releases once the repo is connected. Until then,
build locally with `pnpm tauri build`.

The first signed releases will be **unsigned** until an Apple Developer ID and a Windows EV
code-signing certificate are provisioned. Expect a one-time Gatekeeper / SmartScreen
unsigned-binary warning on first install.

## Telemetry

Aptabase, events only — never prompt content. See spec §12 for the event whitelist.
