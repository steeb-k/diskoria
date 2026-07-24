; Diskoria installer (Inno Setup 6.6.0+ — native dark mode via WizardStyle=modern).
;
; This script is driven from build-release.ps1, which passes the version,
; the signed source exe, the output dir, and the icon as /D defines so Cargo.toml
; stays the single source of truth. Compile manually with, e.g.:
;
;   ISCC.exe /DMyAppVersion=1.5.1 ^
;            /DMySourceExe=..\releases\1.5.1\diskoria.exe ^
;            /DMyOutputDir=..\releases\1.5.1 ^
;            /DMyIconFile=..\assets\appicon2.ico ^
;            installer\diskoria.iss

#ifndef MyAppVersion
  #error MyAppVersion must be supplied via /DMyAppVersion=<x.y.z>
#endif
#ifndef MySourceExe
  #error MySourceExe must be supplied via /DMySourceExe=<path to signed diskoria.exe>
#endif
#ifndef MyOutputDir
  #error MyOutputDir must be supplied via /DMyOutputDir=<output dir>
#endif
#ifndef MyIconFile
  #error MyIconFile must be supplied via /DMyIconFile=<path to .ico>
#endif

#define MyAppName "Diskoria"
#define MyAppExe  "diskoria.exe"
#define MyPublisher "steeb-k"
#define MyAppUrl  "https://github.com/steeb-k/diskoria-binaries"
; Scheduled-task name — must match crate::autostart::TASK_NAME.
#define MyStartupTask "Diskoria Startup"

[Setup]
; Stable AppId across versions → in-place upgrades and a single uninstall entry.
AppId={{B7E4C9A2-3F61-4D8E-9A2B-7C5E1F0D4A86}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyPublisher}
AppPublisherURL={#MyAppUrl}
AppSupportURL={#MyAppUrl}
AppUpdatesURL={#MyAppUrl}
VersionInfoVersion={#MyAppVersion}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern dynamic
SetupIconFile={#MyIconFile}
UninstallDisplayIcon={app}\{#MyAppExe}
; modern look + "dynamic" → follow the Windows light/dark system setting (6.6.0+).
; Use "modern dark" to force dark regardless of system.
Compression=lzma2/max
SolidCompression=yes
OutputDir={#MyOutputDir}
OutputBaseFilename=diskoria-{#MyAppVersion}-setup
; Let the in-app updater's exiting exe be replaced without a manual prompt.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
; Checked by default → installed builds launch at startup out of the box. The
; portable exe never creates this task, so it defaults OFF there.
Name: "startup"; Description: "Launch Diskoria at startup (minimized to the tray)"; GroupDescription: "Startup:"

[Files]
Source: "{#MySourceExe}"; DestDir: "{app}"; DestName: "{#MyAppExe}"; Flags: ignoreversion

[Registry]
; Marks this as an *installed* build. crate::install_mode reads InstallDir and
; compares it to the running exe's directory: matching → "Installed" (tray-on-close
; defaults ON), anything else — including a copy of the exe carried elsewhere on a
; machine that also has Diskoria installed → "Portable" (defaults OFF).
;
; uninstalldeletevalue, NOT uninstalldeletekey: HKLM\Software\Diskoria is shared —
; older builds already store AutoCheckUpdates there — so uninstall must remove only
; the value we own, not the whole key. Deleting InstallDir is enough for detection
; to fall back to "Portable".
Root: HKLM; Subkey: "Software\{#MyAppName}"; ValueType: string; ValueName: "InstallDir"; ValueData: "{app}"; Flags: uninstalldeletevalue

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
; {commondesktop} = the all-users / public Desktop (C:\Users\Public\Desktop).
Name: "{commondesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; Tasks: desktopicon

[Run]
; Register the launch-at-startup task when the box is left checked. Because the
; app is requireAdministrator, we use a Scheduled Task with /RL HIGHEST so it
; starts elevated at logon WITHOUT a UAC prompt (a Run-key entry would prompt).
; runascurrentuser → schtasks runs with Setup's elevated token; the doubled ""
; and \" produce a command line of:
;   /TR "\"{app}\diskoria.exe\" --minimized"
; so a spaced install path stays a single token.
Filename: "schtasks.exe"; \
  Parameters: "/Create /TN ""{#MyStartupTask}"" /TR ""\""{app}\{#MyAppExe}\"" --minimized"" /SC ONLOGON /RL HIGHEST /F"; \
  Flags: runhidden runascurrentuser; Tasks: startup; StatusMsg: "Registering startup task..."
; runascurrentuser → launch with Setup's already-elevated token so the app's
; requireAdministrator manifest is honored (no second UAC prompt). Without it,
; postinstall entries run de-elevated as the original user.
Filename: "{app}\{#MyAppExe}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent runascurrentuser

[UninstallRun]
; Remove the startup task on uninstall (harmless if it was never created).
Filename: "schtasks.exe"; Parameters: "/Delete /TN ""{#MyStartupTask}"" /F"; Flags: runhidden; RunOnceId: "DelStartupTask"
