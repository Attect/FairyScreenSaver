# Builds the release binary and stages it as a Windows screen saver.
#
#   powershell -ExecutionPolicy Bypass -File package.ps1
#
# Windows treats any executable renamed to ".scr" as a screen saver; nothing
# else is required.  The script also stages the `.scr` next to the binary so
# the settings page's "Install" action can pick it up.

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$dist = Join-Path $root 'dist'

Write-Host '==> cargo build --release'
Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}

# Ask cargo where it actually puts artifacts.  It honours CARGO_TARGET_DIR, a
# local .cargo/config.toml, and the platform default -- guessing any of those
# here would break on somebody else's machine.
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else {
    (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).target_directory
}
$exe = Join-Path $targetDir 'release\FairyScreenSaver.exe'
if (-not (Test-Path $exe)) { throw "release binary not found at $exe" }

New-Item -ItemType Directory -Force -Path $dist | Out-Null

$scr = Join-Path $dist 'FairyScreenSaver.scr'
Copy-Item -Force $exe $scr
Copy-Item -Force $exe (Join-Path $dist 'FairyScreenSaver.exe')

$size = [math]::Round((Get-Item $scr).Length / 1KB)
Write-Host ""
Write-Host "==> staged $scr ($size KB)"
Write-Host ""
Write-Host "Install (pick one):"
Write-Host "  1. Right-click dist\FairyScreenSaver.scr  ->  Install"
Write-Host "  2. Copy it to  $env:WINDIR\System32  (needs an elevated shell)"
Write-Host "  3. Double-click dist\FairyScreenSaver.scr to open the settings dialog"
Write-Host ""
Write-Host "Test without installing:"
Write-Host "  dist\FairyScreenSaver.exe /s      run the saver"
Write-Host "  dist\FairyScreenSaver.exe /c      open the settings dialog"
Write-Host "  dist\FairyScreenSaver.exe /d      write a diagnostics dump to the log"
