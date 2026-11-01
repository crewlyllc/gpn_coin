@echo off
REM ============================================================================
REM  GPN installer wrapper (Windows).
REM
REM  Double-clicking a .ps1 opens Notepad by default, so this is the thing users
REM  actually run. It forwards every argument to install.ps1, including
REM  -Uninstall / --remove-autostart and -WhatIf.
REM
REM    install.cmd                      install + start + autostart
REM    install.cmd -Uninstall           remove the autostart (keeps data/wallet)
REM    install.cmd -WhatIf              show what would happen, change nothing
REM ============================================================================
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
exit /b %ERRORLEVEL%
