# RDP setup (Mac → Windows VM)

Prompt Player supports two RDP architectures.

## Architecture A — Host-side typing (default, ~95% of cases)

Just install Prompt Player on your Mac and use any of these RDP/VM clients:

- Microsoft Remote Desktop (`com.microsoft.rdc.macos`)
- Parallels Desktop (`com.parallels.desktop.console`)
- VMware Fusion (`com.vmware.fusion`)
- Citrix Workspace (`com.citrix.receiver.icaclient`)

When the foreground app is one of these, Prompt Player automatically:
- Adds a 30 ms inter-key floor (RDP clients drop bursts).
- Slows typing by ×1.3.
- Disables the clipboard fallback (RDP clipboard sync is unreliable).
- Coalesces backspaces into single events.

You can edit the recognized-client list in **Settings → RDP clients**.

## Architecture B — Guest-side helper daemon (optional)

For high-latency RDP sessions, complex Unicode, or IME-heavy languages,
install the tiny guest helper inside the Windows VM:

1. Download `prompt-player-guest-helper.msi` from GitHub Releases.
2. Run the installer in the Windows VM (no admin needed).
3. The daemon starts automatically on `127.0.0.1:9847`.
4. A shared secret is generated at `%APPDATA%\PromptPlayer-GuestHelper\secret`
   and locked to the current user via `icacls`.
5. In the Mac app **Settings → RDP**, enable "Use guest helper" and paste the
   secret. (The Mac app will offer to do this automatically when it detects
   host-side typing failing.)

When the guest helper is connected, Prompt Player sends the schedule over the
TCP connection; the daemon types locally inside the VM. More reliable; same
human cadence.

## Limitations

- If you RDP **into** the Mac from elsewhere, behavior is untested.
- Multi-monitor RDP setups: Prompt Player follows the foreground window only.
