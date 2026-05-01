# Prompt Player

Stealth keyboard utility for live demos. Type the first word of a stored prompt + `>`,
and the app suppresses the `>` and continues typing the rest with statistically realistic
human cadence — log-normal mixture distribution, hierarchical pauses, occasional typos with
corrections, the works.

See [`prompt-player-spec.md`](./prompt-player-spec.md) for the full v0.3 specification.

## Status

Active build, following the 14-phase plan in `prompt-player-spec.md` §16.

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
