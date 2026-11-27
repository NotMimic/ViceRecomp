@echo off
rem VICTORY GRIMKAAAH: builds Victory from source\ and puts the .exe files in bin\ (tools\build.ps1).
rem Only after a change of the code in source\. Then: PREPARE.bat.
title Victory Grimkaaah - build
powershell -NoProfile -ExecutionPolicy RemoteSigned -File "%~dp0tools\build.ps1"
echo.
pause
