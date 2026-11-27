@echo off
rem VICTORY GRIMKAAAH: translates the shaders and makes the pipelines with our settings (tools\prepare.ps1).
rem To do after BUILD.bat; Victory Grimkaaah.bat says when it is needed.
rem A menu: Quick (a few minutes) or Full (every pipeline of the title, long).
title Victory Grimkaaah - preparation
powershell -NoProfile -ExecutionPolicy RemoteSigned -File "%~dp0tools\prepare.ps1"
echo.
pause
