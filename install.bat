@echo off
rem Double-click to install; "install.bat -Uninstall" removes the plugin.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
