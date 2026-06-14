@echo off
setlocal EnableExtensions
if not defined AGENT_TUI_PROJECT set "AGENT_TUI_PROJECT=D:\mem0"
set "BIN=%~dp0..\target\release\agent-tui.exe"
if not exist "%BIN%" set "BIN=%~dp0..\target\debug\agent-tui.exe"

if "%~2"=="" (
    echo 用法: notify ^<agent^> ^<消息^>
    echo   notify claude codex 已改完 auth，请 review
    echo Agent 发消息请加 --from: agent-tui notify claude "msg" --from mimo
    exit /b 1
)

set "AGENT=%~1"
shift
"%BIN%" notify "%AGENT%" %* --project-dir "%AGENT_TUI_PROJECT%"
endlocal
