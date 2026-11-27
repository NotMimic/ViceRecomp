@echo off
rem ViceRecomp: starts the launcher (tools\play.ps1). Menu: 1 Quality (ray tracing), 2 Fluidity,
rem 3 Diagnostic, 4 Quit. The settings are in tools\settings.ps1, the logs in logs\.
rem Start it from a normal window, not as administrator.
title ViceRecomp
powershell -NoProfile -ExecutionPolicy RemoteSigned -File "%~dp0tools\play.ps1" %*
echo.
pause
