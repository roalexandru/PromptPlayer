# Permissions

## macOS

Prompt Player needs **two** permissions:

| Permission | Why | Where to grant |
|---|---|---|
| **Accessibility** | Inject keystrokes (`CGEventPost`) and detect foreground app | System Settings → Privacy & Security → Accessibility |
| **Input Monitoring** | Receive global keystrokes via `CGEventTap` (the trigger detector) | System Settings → Privacy & Security → Input Monitoring |

**It does NOT need Screen Recording.** It will never request it.

### After upgrading the app

If you upgrade and triggers stop firing, the bundle ID is **stable across releases**
(`com.roalexandru.promptplayer`), so you should NOT need to re-approve. If something is
stuck, the app surfaces a "Reset & Reapprove" button in Settings that runs:

```
tccutil reset Accessibility com.roalexandru.promptplayer
```

then walks you back through approval.

### Secure Input

When you focus a password field (1Password, Keychain, `sudo` in Terminal), macOS
engages **Secure Event Input** which legally blocks any app from reading or
suppressing your keystrokes. Prompt Player detects this and:

- Disables trigger detection (everything passes through).
- Shows a 🔒 icon in the tray.
- Logs telemetry — content-free, just the event.

This is by design; trying to bypass it would be a security hole.

## Windows

No special permissions for standard user.

**Caveat:** `SetWindowsHookEx` doesn't intercept keystrokes in apps running
**as Administrator**. To support admin-elevated targets we'd need to ship a
UI Access manifest variant (deferred to v2 per spec §9.2).
