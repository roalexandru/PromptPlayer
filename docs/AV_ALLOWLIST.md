# Antivirus allowlist (Windows)

Any tool that installs a low-level keyboard hook (`SetWindowsHookEx`,
`WH_KEYBOARD_LL`) and synthesizes keystrokes (`SendInput`) is heuristically
flagged by AV engines until vendor-side reputation builds up. This is normal
for the category — Beeftext, AutoHotkey, Espanso, etc. all hit the same wall
and recover via reputation + vendor outreach.

## Status

- **EV code-signing certificate:** not yet provisioned. First signed releases
  will dramatically reduce SmartScreen and Defender friction.
- **Vendor allowlists:** outreach starts after we hit ~1k installs (per
  spec §9.2). Vendors covered: Microsoft Defender, Symantec, BitDefender,
  Kaspersky.

## What Prompt Player does (and doesn't) do

- ✅ Installs a `WH_KEYBOARD_LL` hook to detect the trigger commit char.
- ✅ Suppresses the commit char from reaching the focused app.
- ✅ Synthesizes keystrokes via `SendInput` when typing the prompt body.
- ❌ Does NOT log keystrokes anywhere.
- ❌ Does NOT exfiltrate text. The only network traffic is Aptabase telemetry
  (event names + small enums; never prompt content).
- ❌ Does NOT modify other processes' memory or files.
- ❌ Does NOT install a service or driver. Runs as a normal user-mode app.

## If your AV flags it

Most AV tools have a **per-vendor** allowlist mechanism. Add the binary to
your exception list:

- Defender: Settings → Update & Security → Windows Security → Virus & threat
  protection → Manage settings → Exclusions.
- Symantec, BitDefender, Kaspersky: similar UX in their respective consoles.

If your IT department runs centrally-managed policy, point them to this
document and the source code on GitHub for review.
