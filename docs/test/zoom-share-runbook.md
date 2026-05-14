# Zoom screen-share + HDMI mirror — manual test runbook

This runbook backs the fix for "picker doesn't function during Zoom screen share." Some failure modes (DWM compositor bugs, what a remote viewer actually sees, GPU mirror output) can't be automated — they need a real second machine, a real Zoom call, and a real HDMI display. Run this whenever:

- You touch `picker/window.rs`, `commands/picker.rs`, or `platform/windows/capture.rs`.
- You bump `tauri`, `windows`, `webview2-com`, or any GPU/compositor-adjacent dep.
- You upgrade to a new Windows feature release (the bug surfaced on 24H2).

Record the date, your Windows build, and the Zoom version at the top of each run. Failures are not "yellow" — either it works or the fix has regressed.

---

## Environment

| Field | Value |
|---|---|
| Run date | _e.g._ 2026-05-13 |
| Tester | |
| Prompt Player version | _from About menu_ |
| Windows version | `winver` → e.g. `Win 11 Pro 24H2 (26100.xxxx)` |
| GPU + driver | `dxdiag` → e.g. `NVIDIA RTX 4060, 552.22` |
| Zoom version | Help → About → e.g. `6.3.10 (44781)` |
| HDMI display model | _used in §3 only_ |

---

## Build & install the candidate

The fix only matters in the installed MSI build (release WebView2 path differs from `cargo tauri dev`). Always test against the MSI:

```pwsh
cd C:\promptPlayer
pnpm install
pnpm tauri build
# Uninstall the previous copy via Settings → Apps → "Prompt Player"
# Then double-click src-tauri\target\release\bundle\msi\*.msi
```

Verify install: tray icon visible, `%LOCALAPPDATA%\PromptPlayer\logs\prompt-player.log` exists and has a fresh `boot state: hook_alive=true` line.

---

## §1 — Functional regression (no sharing, single display)

Pre-condition: no Zoom call active, no other screen-capture tool (OBS, Teams, Discord) running.

| # | Step | Expected | ✅ / ❌ |
|---|---|---|---|
| 1 | Click tray icon → "Quit" → re-launch app from Start menu | Tray icon comes back. Log shows `single-instance` activation if you double-launched. | |
| 2 | Focus Notepad. Press the trigger character (default `>>`) | Picker opens, search input is focused. Notepad still has its caret but is no longer foreground. | |
| 3 | Type a query in the picker, press Enter | Picker hides, query's prompt types into Notepad at the cursor. | |
| 4 | Repeat 2–3 with Shift+Enter (paste mode) | Same target receives the prompt via clipboard paste. Clipboard contents you had before are restored within ~200ms (test: copy "abc" first, fire a prompt, then Ctrl+V again — should still paste "abc"). | |
| 5 | Press the trigger then Escape | Picker hides, focus returns to Notepad, no characters leaked. | |

Any ❌ in §1 → **stop**, don't proceed to Zoom tests. This is the cold-start regression check.

---

## §2 — Zoom screen-share scenarios

Recruit a co-worker to join your Zoom call as a passive viewer. They'll confirm what they actually see vs. what you see locally. Without a second viewer, screenshot Zoom's "Self-view" capture preview (Settings → Share Screen → Preview) but treat that as best-effort, not authoritative.

### §2.1 — Full-screen share, picker visible to local user only

| # | Step | Local user sees | Remote viewer sees | ✅ / ❌ |
|---|---|---|---|---|
| 1 | Start Zoom share → "Screen" (full screen) | Sharing-active banner at top of screen | Your desktop | |
| 2 | Press trigger → picker opens | Picker renders normally (palette + search input visible) | Picker should NOT appear — the area where it is shows the desktop behind it (or black, depending on Zoom version) | |
| 3 | Type a query → select a prompt with Enter | Picker hides, prompt types into the underlying target app | Only the typed text appearing in the target app — never the picker UI | |
| 4 | Re-open picker, press Escape | Picker hides cleanly | Nothing visible the whole time | |
| 5 | Stop sharing, re-open picker (no share active) | Picker still renders normally | n/a | |

Per-step pass criteria:
- Step 2 local: if you see "nothing", "black rectangle", "transparent rectangle", or "white rectangle" — that's the regression we're fixing. ❌.
- Step 2 remote: if your co-worker sees the picker — privacy promise broken. ❌.
- Step 3 local: if first few characters land in Zoom's annotation toolbar or vanish — that's RC-2 regression. ❌.

### §2.2 — Specific-window share

| # | Step | Local user sees | Remote viewer sees | ✅ / ❌ |
|---|---|---|---|---|
| 1 | Zoom share → "Window" → pick Notepad | Sharing banner shows "Sharing: Notepad" | Just Notepad contents | |
| 2 | Press trigger → picker opens over Notepad | Picker renders normally | Just Notepad (picker isn't in Notepad's HWND tree, so Zoom never captured it) | |
| 3 | Fire a prompt → text types into Notepad | Prompt types as normal | Prompt appears in Notepad | |

### §2.3 — Stop / restart share resilience

| # | Step | Local user sees | ✅ / ❌ |
|---|---|---|---|
| 1 | Start share, open picker, close picker, **stop share** | Picker open and close work fine before and after | |
| 2 | Re-start share, re-open picker | Picker still renders correctly | |
| 3 | While share is active, hide/show picker 5 times rapidly | All 5 cycles render cleanly, no flicker/leak | |

---

## §3 — HDMI mirror / presenter mode

HDMI mirror = the GPU duplicates one monitor's output to another physical display, below DWM. `WDA_EXCLUDEFROMCAPTURE` does not affect this path — both displays should show identical content, including the picker. The fix must not break that.

### §3.1 — Duplicate mode (presenter has same view as projector)

| # | Step | Laptop screen | HDMI/projector | ✅ / ❌ |
|---|---|---|---|---|
| 1 | `Win+P` → "Duplicate", plug in HDMI | Same content on both | Same content on both | |
| 2 | Open picker (no Zoom share) | Picker visible | Picker visible (this is fine — the audience here is in the room, watching the projector) | |
| 3 | Start Zoom share → "Screen" (full screen) | Sharing banner visible | Sharing banner visible | |
| 4 | Open picker over a target app | Picker visible | Picker visible (HDMI mirror is below DWM) | |
| 5 | Picker visible on Zoom share? | n/a | (have a remote viewer confirm) — must be **invisible** to the Zoom feed even though the local HDMI shows it | |
| 6 | Fire a prompt | Types into target on laptop screen | Types into target on HDMI mirror | |

### §3.2 — Extend mode + share only external display

| # | Step | Laptop screen | HDMI/projector | Zoom feed | ✅ / ❌ |
|---|---|---|---|---|---|
| 1 | `Win+P` → "Extend", plug in HDMI | Desktop on laptop | Empty/extended desktop on HDMI | n/a | |
| 2 | Move Slack/Notepad to the HDMI monitor. Start Zoom share → "Screen 2" (the HDMI one) | Laptop desktop | Slack/Notepad | The HDMI monitor's contents | |
| 3 | Press trigger ON THE LAPTOP screen (where the picker should open by default — it tracks cursor) | Picker on laptop screen | unchanged (picker is on the other monitor) | Picker should NOT appear (it's on a different monitor than the one being shared anyway) | |
| 4 | Now move cursor to HDMI screen, fire trigger there | Laptop unchanged | Picker on HDMI | Picker should NOT appear in Zoom share | |
| 5 | Fire a prompt from §3.2.4 | Picker types into the app on HDMI monitor | Prompt appears | Prompt appears (the typed result IS shared, the picker UI is not) | |

§3.2 step 4 is the critical interaction: picker on a SHARED monitor must still be hidden from the Zoom feed (Layer B), but visible to the local user.

---

## §4 — Log inspection

After running §1–§3, open `%LOCALAPPDATA%\PromptPlayer\logs\prompt-player.log` and confirm:

- `display-affinity applied to picker tree` lines exist, each with `applied >= 2` (parent + at least one WebView2 descendant). Zero would mean only the parent got the flag — the regression.
- `display-affinity set (descendant)` debug lines list `Chrome_WidgetWin_*` or similar WebView2 class names, not just the parent `Tauri Window`.
- No `WARN` lines about `SetWindowDisplayAffinity on parent failed`.
- `capture_foreground` (Windows) snapshot log lines show the target app's class (e.g., `Notepad`, `Chrome_WidgetWin_1`), **not** any `ZP*` Zoom-helper class even during an active share.
- `wait_until_foreground` outcomes never hit the 400ms timeout cap (`elapsed_ms` always < 400). One or two cap-hits during heavy load is acceptable; if every fire times out, focus restore has regressed.

Paste the relevant log block into the run record so future debugging has a reference.

---

## §5 — Automated test suite

Always run before signing off:

```pwsh
cd C:\promptPlayer\src-tauri
cargo test --lib                              # T1 pure-function + T3/T4 in-source tests
cargo test --test screen_capture_exclusion    # T2 — Win32 HWND-tree integration tests
cargo clippy --all-targets -- -D warnings
```

Expected: T2 cases A/B/C/D/E all green. Any T2 failure means the recursive walk in `capture.rs::apply_affinity_recursive` is broken — DO NOT ship.

---

## §6 — Result

| Section | Pass / Fail | Notes |
|---|---|---|
| §1 — Regression (no share) | | |
| §2.1 — Full-screen share | | |
| §2.2 — Window share | | |
| §2.3 — Share resilience | | |
| §3.1 — HDMI duplicate | | |
| §3.2 — HDMI extend + share external | | |
| §4 — Log inspection | | |
| §5 — Automated tests | | |

Sign-off (release-blocking): the above must be all-green for a release build to ship.

---

## §7 — Troubleshooting matrix

Two distinct Windows bugs can cause "picker doesn't function during Zoom share." This PR addresses one of them via code; the other requires deployment-time intervention. Use the log signals below to tell them apart.

### §7.1 — WebView2 child-HWND inheritance (this PR fixes)

**Microsoft-confirmed root cause** for "tooltips and HTML select dropdowns remain visible in screenshots when SetWindowDisplayAffinity is used on a WebView2 host" — WebView2Feedback #4544 (tracked as AB#50877897). Same class of bug surfaces against full screen capture too: the parent HWND gets the flag, the GPU swap-chain child does not, and DWM's compositor mitigation leaves the WebView2 surface blank to the local user.

**Log signal that the fix is engaging:**

```
display-affinity applied to picker tree  applied=4 attempted=4 parent=...
display-affinity set (descendant)  class=Chrome_WidgetWin_1 ...
display-affinity set (descendant)  class=Intermediate D3D Window ...
```

If `applied >= 2` and the descendant classes look WebView2-ish, this branch of the bug is mitigated.

### §7.2 — Win11 `ChangeWindowTreeProtection` kernel bug (this PR cannot fix)

A Microsoft engineer (Junjie Zhu) confirmed a bug in `win32kfull.sys::ChangeWindowTreeProtection` causes `SetWindowDisplayAffinity` to return `ERROR_NOT_ENOUGH_MEMORY` (HRESULT `0x80070008`) for "non-traditional Win32" applications — Chromium, Firefox, Edge, Teams, and (by extension) WebView2-hosted Tauri windows. See [SetWindowDisplayAffinity on Windows 11](https://learn.microsoft.com/en-us/answers/questions/700122/setwindowdisplayaffinity-on-windows-11).

**Log signal:**

```
ERROR  win11_legacy_display_affinity_bug: SetWindowDisplayAffinity returned ERROR_NOT_ENOUGH_MEMORY...
```

**Microsoft's official workaround** (deployment-time, not code-fixable):

1. Install the Windows ADK (Application Compatibility Toolkit).
2. Open Compatibility Administrator (run as admin).
3. File → New → Database → Create New → Application Fix.
4. Target: the installed `Prompt Player.exe` (typically `%ProgramFiles%\Prompt Player\Prompt Player.exe`).
5. Compatibility Modes → Apply Compatibility Fixes → check **LegacyDisplayAffinity**.
6. Save the `.sdb` and install with `sdbinst.exe <file>.sdb` (admin).
7. Restart Prompt Player and re-run §2.1.

A future installer update could ship the `.sdb` automatically. Out of scope for this PR.

### §7.3 — Zoom-side: "Advanced capture with window filtering" must be ON

Zoom Desktop → Settings → Share Screen → check **"Use TCP connection for screen sharing"** AND **"Advanced capture with window filtering"** (sometimes labeled "Capture only specific windows in screen share"). When this is OFF, Zoom uses a capture path that bypasses `SetWindowDisplayAffinity` entirely — no amount of code on our side will hide the picker. This is documented in the [Cluely / Adam Svoboda writeup](https://adamsvoboda.net/how-interview-cheating-tools-hide-from-zoom/).

**Pre-flight checklist for §2/§3:** confirm both Zoom settings are ON before testing. Note the Zoom version when recording the run (the setting's label has moved between 5.x and 6.x).

---

## Appendix — known caveats

- **Zoom 5.x vs 6.x**: 5.x is more lenient about `WDA_EXCLUDEFROMCAPTURE`. The bug repros most reliably on 6.x + Win11 24H2 + an integrated GPU. If you can't repro on a dGPU machine, that's still informative — note it.
- **Teams equivalent**: Teams share uses the same Graphics.Capture path; if Zoom passes, Teams should too. Smoke-test it if you have time.
- **OBS / NDI**: these use a third capture path. Not in the in-scope test set; record any anomalies for follow-up.
- **WebView2 Evergreen updates**: Microsoft can ship a new Edge that regresses this. If a user reports the bug back after a previously-passing release, the first thing to ask for is their `Edge://version` (specifically the WebView2 runtime version) along with logs.
