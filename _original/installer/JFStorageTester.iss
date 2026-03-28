; JF Storage Tester - Inno Setup Script
; Copyright (c) 2026 JF

#define MyAppName "JF Storage Tester"
#define MyAppVersion "1.0.4.2"
#define MyAppPublisher "JF"
#define MyAppExeName "JFStorageTester.exe"

[Setup]
; NOTE: The value of AppId uniquely identifies this application.
AppId={{8F4E9B2A-3C7D-4E5F-A6B8-1D2E3F4A5B6C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
; Output settings
OutputDir=..\installer_output
OutputBaseFilename=JFStorageTester_Setup_{#MyAppVersion}
; Compression
Compression=lzma2/ultra64
SolidCompression=yes
; Installer appearance
WizardStyle=modern
; Require admin for installation (app needs admin anyway)
PrivilegesRequired=admin
; Windows version requirement
MinVersion=10.0
; Uninstaller
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
; Close running instances before installing (critical for updates)
CloseApplications=force
CloseApplicationsFilter=*.exe
RestartApplications=no
AppMutex=JFStorageTester_SingleInstance

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\publish\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
; Interactive installs: show checkbox to launch app
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent shellexec runascurrentuser
; Silent installs (auto-update): always launch the app
Filename: "{app}\{#MyAppExeName}"; Flags: nowait skipifnotsilent shellexec runascurrentuser



