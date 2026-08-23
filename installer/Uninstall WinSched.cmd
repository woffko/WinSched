@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$process = Start-Process PowerShell.exe -Verb RunAs -Wait -PassThru -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File ""%~dp0uninstall.ps1""'; exit $process.ExitCode"
set "WIN_SCHED_EXIT=%ERRORLEVEL%"
if not "%WIN_SCHED_EXIT%"=="0" pause
exit /b %WIN_SCHED_EXIT%
