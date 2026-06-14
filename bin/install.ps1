# Add agents.cmd to user PATH (run once)
$ErrorActionPreference = "Stop"
$BinDir = (Resolve-Path $PSScriptRoot).Path
$ReleaseExe = Join-Path (Split-Path $BinDir) "target\release\agent-tui.exe"

if (-not (Test-Path $ReleaseExe)) {
    Write-Host "Build first: cd D:\kimi\agent-tui; cargo build --release" -ForegroundColor Yellow
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$parts = @($userPath -split ";" | Where-Object { $_ -and $_ -ne "" })

# Remove mistaken entries (e.g. install.ps1 file path)
$parts = @($parts | Where-Object {
    $_ -ne $BinDir -and
    $_ -notlike "*\install.ps1" -and
    $_ -notlike "*\agents.cmd"
})

$parts += $BinDir
$newPath = ($parts -join ";")
[Environment]::SetEnvironmentVariable("Path", $newPath, "User")
$env:Path = "$env:Path;$BinDir"
Write-Host "PATH updated: $BinDir" -ForegroundColor Green

if (-not [Environment]::GetEnvironmentVariable("AGENT_TUI_PROJECT", "User")) {
    [Environment]::SetEnvironmentVariable("AGENT_TUI_PROJECT", "D:\mem0", "User")
    $env:AGENT_TUI_PROJECT = "D:\mem0"
    Write-Host "AGENT_TUI_PROJECT=D:\mem0"
}

Write-Host ""
Write-Host "Restart PowerShell, then run:" -ForegroundColor Green
Write-Host "  agents          (5-pane grid, cursor lead)"
Write-Host "  agents cursor   (grid, focus cursor)"
Write-Host "  agents solo     (single-agent fullscreen)"
Write-Host "  Ctrl+Tab  switch agent,  F2  grid or solo"
