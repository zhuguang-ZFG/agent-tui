@echo off
setlocal EnableExtensions
if not defined AGENT_TUI_PROJECT set "AGENT_TUI_PROJECT=D:\mem0"
set "BIN=%~dp0..\target\release\agent-tui.exe"
if not exist "%BIN%" set "BIN=%~dp0..\target\debug\agent-tui.exe"

if "%~2"=="" (
    echo 用法: delegate ^<worker^> ^<task^> [说明...]
    echo   delegate codex auth-api 实现登录 API
    exit /b 1
)

set "WORKER=%~1"
set "TASK=%~2"
shift
shift
set "DESC=%*"
if defined DESC (
    "%BIN%" delegate "%WORKER%" "%TASK%" --description "%DESC%" --project-dir "%AGENT_TUI_PROJECT%"
) else (
    "%BIN%" delegate "%WORKER%" "%TASK%" --project-dir "%AGENT_TUI_PROJECT%"
)
endlocal
