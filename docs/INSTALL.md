# Installing Prompt Player

## macOS

1. Download the latest `.dmg` from the GitHub Releases page (artifacts in the
   release CI workflow until first signed release).
2. Open the `.dmg` and drag **Prompt Player** to `/Applications`.
3. **First launch — Gatekeeper warning.** Because the app is currently
   distributed unsigned (Apple Developer ID coming later), macOS will refuse
   to open it on first double-click. Workaround:
    - Right-click `Prompt Player.app` in `/Applications` → **Open**.
    - macOS shows "Open anyway" — click it.
    - Subsequent launches are unblocked.
4. **Permissions** — see [PERMISSIONS.md](./PERMISSIONS.md). The app needs
   **Accessibility** and **Input Monitoring** to do its job. It does NOT need
   Screen Recording.

## Windows

1. Download the latest `.msi` from the GitHub Releases page.
2. Run the installer.
3. **First launch — SmartScreen warning.** Because the installer is currently
   unsigned (EV cert coming later), Windows Defender SmartScreen will show a
   blue dialog. Workaround:
    - Click **More info** → **Run anyway**.
    - Subsequent launches are unblocked.
4. Standard user permissions are sufficient. Note: keystroke hooks do NOT
   intercept apps running as Administrator without an additional UI Access
   manifest (deferred to v2 per spec §9.2).

## After install

- The tray icon appears.
- The app starts **disarmed**. Click the tray (or `⌘⇧P` / `⌃⇧P`) to arm.
- Drop `.pp.md` files in:
   - macOS: `~/Library/Application Support/PromptPlayer/prompts/`
   - Windows: `%APPDATA%\PromptPlayer\prompts\`
- Two example prompts ship with the app (`intro`, `refactor-to-async`).

## Antivirus

If your AV flags the binary, see [AV_ALLOWLIST.md](./AV_ALLOWLIST.md).
This is expected for any keyboard-hook tool until vendor outreach completes.
