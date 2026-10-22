@echo off
REM ───────────────────────────────────────────────────────────────────────────
REM  GPN node — open the local dashboard.
REM
REM  Starts the supervisor with `--gui` (if it is not already running) and
REM  opens http://127.0.0.1:8099 in the default browser. The dashboard is a
REM  LOOPBACK-ONLY control panel: it can idle/resume mining, so it never
REM  listens on the network.
REM
REM  Put a shortcut to this file on the Desktop (right-click → Send to →
REM  Desktop (create shortcut)) and rename it "GPN Node".
REM
REM  Idle is NOT "turn GPN off": during an idle window the node keeps syncing,
REM  peering and serving the chain, while mining and AI job claims pause and
REM  resume automatically at the deadline (max 6 hours).
REM ───────────────────────────────────────────────────────────────────────────
setlocal
set "HERE=%~dp0"
set "BIN=%HERE%bin"
set "GPN_GUI=1"
set "GPN_GUI_LISTEN=127.0.0.1:8099"

if not exist "%BIN%\gpn_node.exe" (
  REM Fall back to a bundle unpacked one level up (…\gpn\bin) or on PATH.
  if exist "%HERE%..\bin\gpn_node.exe" (
    set "BIN=%HERE%..\bin"
  ) else (
    where gpn_node.exe >nul 2>nul || (
      echo Could not find gpn_node.exe.
      echo Unpack the Windows release bundle next to this file ^(in .\bin\^),
      echo or add its folder to PATH, then run this again.
      pause
      exit /b 1
    )
    set "BIN="
  )
)

REM Start the supervisor with the dashboard enabled, minimised, in the
REM background — but only if it is not already up (tasklist check keeps this
REM idempotent, so the shortcut can be clicked twice).
tasklist /fi "imagename eq gpn_node.exe" | find /i "gpn_node.exe" >nul
if errorlevel 1 (
  if defined BIN (
    start "GPN node" /min "%BIN%\gpn_node.exe" --gui
  ) else (
    start "GPN node" /min gpn_node.exe --gui
  )
  REM Give the supervisor a moment to bind the dashboard.
  timeout /t 4 /nobreak >nul
)

start "" "http://127.0.0.1:8099"
endlocal
