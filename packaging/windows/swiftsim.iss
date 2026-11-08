; Inno Setup script for the SwiftSim installer. Build swiftsim.exe first (cargo build --release),
; then: iscc packaging\windows\swiftsim.iss  -> dist\SwiftSim-setup.exe
#define Exe "..\..\target\release\swiftsim.exe"
#define AppVersion GetVersionNumbersString(AddBackslash(SourcePath) + Exe)

[Setup]
AppId={{45F572AB-AD92-41E5-BE75-E0DBBC0F4FDA}
AppName=SwiftSim
AppVersion={#AppVersion}
AppPublisher=SwiftSim contributors
DefaultDirName={autopf}\SwiftSim
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesAssociations=yes
OutputDir=..\..\dist
OutputBaseFilename=SwiftSim-setup
SetupIconFile=..\..\assets\swiftsim.ico
UninstallDisplayIcon={app}\swiftsim.exe
WizardStyle=modern
Compression=lzma2
SolidCompression=yes

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Exe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-MIT"; DestDir: "{app}"
Source: "..\..\LICENSE-APACHE"; DestDir: "{app}"
Source: "..\..\LICENSE-OFL"; DestDir: "{app}"
Source: "..\..\README.md"; DestDir: "{app}"

[UninstallDelete]
; the previous version, moved aside by the in-app updater
Type: files; Name: "{app}\swiftsim.exe.old"

[Icons]
Name: "{autoprograms}\SwiftSim"; Filename: "{app}\swiftsim.exe"
Name: "{autodesktop}\SwiftSim"; Filename: "{app}\swiftsim.exe"; Tasks: desktopicon

[Registry]
; HKA = HKCU for a per-user install, HKLM for an all-users install.
Root: HKA; Subkey: "Software\Classes\.sws"; ValueType: string; ValueName: ""; ValueData: "SwiftSim.Schematic"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\SwiftSim.Schematic"; ValueType: string; ValueName: ""; ValueData: "SwiftSim schematic"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SwiftSim.Schematic\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\swiftsim.exe,0"
Root: HKA; Subkey: "Software\Classes\SwiftSim.Schematic\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\swiftsim.exe"" ""%1"""

[Run]
Filename: "{app}\swiftsim.exe"; Description: "{cm:LaunchProgram,SwiftSim}"; Flags: nowait postinstall skipifsilent
