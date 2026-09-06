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

You can extend the recognized-client list with `rdp-clients:` in
`promptplayer.yaml` — bundle ids on macOS, executable basenames on Windows.
There is no settings UI for it.

## Architecture B — Guest-side helper daemon (not yet wired up)

> **Status: not usable.** The `guest-helper/` crate in this repository is a
> working daemon — it listens on `127.0.0.1:9847`, authenticates with a shared
> secret and replays a typing schedule with `enigo` — but **nothing in the app
> connects to it**. There is no client, no settings toggle and no released
> installer. Host-side typing (Architecture A) is the only path that works.
>
> The design is kept here because the problem it solves is real: on
> high-latency links, or with IME-heavy input, replaying the schedule inside
> the guest is more faithful than typing across the wire. Wiring it up means
> adding a client to the host, a way to enter the secret, and an installer
> target in the release workflow.

## Limitations

- If you RDP **into** the Mac from elsewhere, behavior is untested.
- Multi-monitor RDP setups: Prompt Player follows the foreground window only.
