# E2E smoke for the Windows MSI bundle. Verifies installer + binary metadata
# and that the app launches cleanly.
#
# Args:
#   -ExpectedVersion (default: read from package.json)
#   -Target (default: x86_64-pc-windows-msvc)
#
# Sets PROMPT_PLAYER_E2E=1 during launch so telemetry is dropped — CI launches
# must not pollute real-user metrics.

param(
  [string]$ExpectedVersion = "",
  [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrEmpty($ExpectedVersion)) {
  $ExpectedVersion = (node -p "require('./package.json').version").Trim()
}

$BundleDir = "src-tauri/target/$Target/release/bundle"
$MsiDir = "$BundleDir/msi"
$ExeAfterBuild = "src-tauri/target/$Target/release/prompt-player.exe"

Write-Host "==> E2E target: $Target, expected version: $ExpectedVersion"

# 1. MSI exists and is non-empty
$Msi = Get-ChildItem -Path $MsiDir -Filter "Prompt Player_${ExpectedVersion}_*.msi" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $Msi -or $Msi.Length -eq 0) {
  Write-Host "::error::missing or empty MSI in $MsiDir"
  Get-ChildItem $MsiDir -ErrorAction SilentlyContinue
  exit 1
}
Write-Host "  [ok] MSI: $($Msi.Name) ($($Msi.Length) bytes)"

# 2. Built exe exists alongside (pre-bundle binary, used for launch test)
if (-not (Test-Path $ExeAfterBuild)) {
  Write-Host "::error::missing built exe at $ExeAfterBuild"
  exit 1
}
$ExeItem = Get-Item $ExeAfterBuild
Write-Host "  [ok] exe: $($ExeItem.Name) ($($ExeItem.Length) bytes)"

# 3. Exe version matches package.json
$VersionInfo = $ExeItem.VersionInfo
$ProductVersion = $VersionInfo.ProductVersion
if ($ProductVersion -notmatch "^${ExpectedVersion}") {
  Write-Host "::error::ProductVersion drift — exe=$ProductVersion, expected=$ExpectedVersion"
  exit 1
}
Write-Host "  [ok] ProductVersion = $ProductVersion"

# 4. Icon resource present in exe (windows embeds the icon).
# Use Get-FileHash as proxy for "non-empty"; the exe should be >5MB for a Tauri build.
if ($ExeItem.Length -lt 5MB) {
  Write-Host "::error::exe suspiciously small ($($ExeItem.Length) bytes) — likely missing resources"
  exit 1
}
Write-Host "  [ok] exe size sanity ($([math]::Round($ExeItem.Length / 1MB, 1)) MB)"

# 5. Bundle ID smoke (read from tauri.conf.json — Win MSI metadata is harder to
# parse without WiX tooling; we already verify config matches at build time).
$Conf = Get-Content "src-tauri/tauri.conf.json" | ConvertFrom-Json
if ($Conf.identifier -ne "com.roalexandru.promptplayer") {
  Write-Host "::error::bundle ID drift — tauri.conf.json identifier = $($Conf.identifier)"
  exit 1
}
Write-Host "  [ok] identifier = $($Conf.identifier)"

# 6. Launch test — start the built exe directly (avoids needing admin to install
# the MSI in CI). Verify alive after 5s, kill cleanly.
Write-Host "==> Launching with PROMPT_PLAYER_E2E=1"

$env:PROMPT_PLAYER_E2E = "1"
$proc = Start-Process -FilePath $ExeAfterBuild -PassThru `
  -RedirectStandardOutput "$env:TEMP\pp-e2e-stdout.log" `
  -RedirectStandardError  "$env:TEMP\pp-e2e-stderr.log"
Write-Host "  pid=$($proc.Id)"

Start-Sleep -Seconds 5

# Refresh process state — Start-Process returns a snapshot.
$still = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
if (-not $still) {
  Write-Host "::error::process exited within 5s"
  Write-Host "---- stdout ----"
  if (Test-Path "$env:TEMP\pp-e2e-stdout.log") { Get-Content "$env:TEMP\pp-e2e-stdout.log" }
  Write-Host "---- stderr ----"
  if (Test-Path "$env:TEMP\pp-e2e-stderr.log") { Get-Content "$env:TEMP\pp-e2e-stderr.log" }
  exit 1
}
Write-Host "  [ok] process alive after 5s"

# Graceful shutdown — CloseMainWindow first, then Kill.
$still.CloseMainWindow() | Out-Null
Start-Sleep -Seconds 2
$still = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
if ($still) {
  Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
}
Write-Host "  [ok] clean shutdown"

Write-Host "==> E2E win PASSED"
