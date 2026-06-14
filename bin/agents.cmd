@echo off
setlocal EnableExtensions
if not defined AGENT_TUI_PROJECT set "AGENT_TUI_PROJECT=D:\mem0"
set "BIN=%~dp0..\target\release\agent-tui.exe"
if not exist "%BIN%" set "BIN=%~dp0agent-tui.exe"
if not exist "%BIN%" set "BIN=%~dp0..\target\debug\agent-tui.exe"

rem Default: 4-pane grid + all enabled agents. Use "agents solo" for single-agent fullscreen.
set "EXTRA="

if /i "%~1"=="solo" (
    set "EXTRA=--solo"
    shift
)
if /i "%~1"=="grid" shift

if /i "%~1"=="claude" (set "EXTRA=%EXTRA% --agent claude" & shift)
if /i "%~1"=="codex" (set "EXTRA=%EXTRA% --agent codex" & shift)
if /i "%~1"=="mimo" (set "EXTRA=%EXTRA% --agent mimo" & shift)
if /i "%~1"=="kimi" (set "EXTRA=%EXTRA% --agent kimi" & shift)
if /i "%~1"=="cursor" (set "EXTRA=%EXTRA% --agent cursor" & shift)

"%BIN%" --project-dir "%AGENT_TUI_PROJECT%" %EXTRA%
endlocal
