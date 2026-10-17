; Inno Setup script for tile-rs.
; Build it with scripts\build-installer.ps1 (which passes the version and the paths).
; Requires Inno Setup 6: https://jrsoftware.org/isdl.php

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BinPath
  #define BinPath "..\dist\tile-rs.exe"
#endif
#ifndef IconPath
  #define IconPath ""
#endif

#define AppName "tile-rs"
#define AppPublisher "tile-rs"
#define AppUrl "https://github.com/ponslookspain/tile-rs"

[Setup]
AppId={{7B5E9F1C-2C4A-4E5B-9D3E-6C1F0A2B4D77}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}
AppUpdatesURL={#AppUrl}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\dist
OutputBaseFilename=tile-rs-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#if IconPath != ""
SetupIconFile={#IconPath}
#endif
UninstallDisplayIcon={app}\tile-rs.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "autostart"; Description: "Start tile-rs when I sign in to Windows"; Flags: unchecked

[Files]
Source: "{#BinPath}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\tile-rs"; Filename: "{app}\tile-rs.exe"
Name: "{group}\{cm:UninstallProgram,tile-rs}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\tile-rs"; Filename: "{app}\tile-rs.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "tile-rs"; ValueData: """{app}\tile-rs.exe"" --autostart"; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\tile-rs.exe"; Description: "Start tile-rs now"; Flags: nowait postinstall skipifsilent
