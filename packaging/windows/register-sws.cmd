@echo off
rem Makes .sws files open with the swiftsim.exe next to this script, for the current user (no admin needed).
set "EXE=%~dp0swiftsim.exe"
reg add "HKCU\Software\Classes\.sws" /ve /d "SwiftSim.Schematic" /f >nul
reg add "HKCU\Software\Classes\SwiftSim.Schematic" /ve /d "SwiftSim schematic" /f >nul
reg add "HKCU\Software\Classes\SwiftSim.Schematic\DefaultIcon" /ve /d "\"%EXE%\",0" /f >nul
reg add "HKCU\Software\Classes\SwiftSim.Schematic\shell\open\command" /ve /d "\"%EXE%\" \"%%1\"" /f >nul
echo .sws files now open with %EXE%
pause
