; PDFXI per-user installer (Inno Setup 6).
;
; Required defines (passed by scripts/package.ps1, never absolute paths inside
; this file): StageDir, AppVersion, OutputDir.
; Optional: SourceIcon (defaults to the committed app icon), Smoke=1, which
; builds a throw-away installer with a different AppId/AppName so smoke tests
; cannot touch a real installation or its uninstall entry.
;
; No Adobe asset, secret, or private document is installed. PDFium is bundled
; under the BSD-3-Clause licence with its notices in pdfium-licenses\.

#ifndef AppVersion
  #error AppVersion define is required
#endif
#ifndef StageDir
  #error StageDir define is required
#endif
#ifndef OutputDir
  #error OutputDir define is required
#endif
#ifndef SourceIcon
  #define SourceIcon "..\crates\pdfxi-app\assets\pdfxi.ico"
#endif
#ifndef Smoke
  #define Smoke 0
#endif

#if Smoke
  #define MyAppId "{{9E1B7C52-6B4A-4E3D-9F27-9A6D0C1C7A21}"
  #define MyAppName "PDFXI Smoke"
  #define MyDefaultDir "{localappdata}\Programs\PDFXI-Smoke"
#else
  #define MyAppId "{{4C1F0A46-8E5B-4D2A-9C77-2B6A1E9F3D10}"
  #define MyAppName "PDFXI"
  #define MyDefaultDir "{localappdata}\Programs\PDFXI"
#endif

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#AppVersion}
AppVerName={#MyAppName} {#AppVersion}
AppPublisher=PDFXI contributors
AppPublisherURL=https://github.com/pdfxi/pdfxi
AppSupportURL=https://github.com/pdfxi/pdfxi/issues
DefaultDirName={#MyDefaultDir}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=
OutputDir={#OutputDir}
OutputBaseFilename=pdfxi-setup-{#AppVersion}-windows-x86_64
SetupIconFile={#SourceIcon}
UninstallDisplayName={#MyAppName} {#AppVersion}
UninstallDisplayIcon={app}\pdfxi-ui.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes
RestartApplications=no
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#MyAppName}
VersionInfoCompany=PDFXI contributors
[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce
; Optional "Open with" registration only. Never claims the .pdf default handler.
Name: "openwith"; Description: "Add {#MyAppName} to the ""Open with"" list for PDF files"; GroupDescription: "Integration:"; Flags: unchecked

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
; User settings live under {userappdata}\PDFXI and are deliberately not installed
; or removed here.

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\pdfxi-ui.exe"; WorkingDir: "{app}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\pdfxi-ui.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Registry]
; HKCU uninstall entry (per-user install; no elevation).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\{#MyAppId}_is1"; ValueType: string; ValueName: "DisplayName"; ValueData: "{#MyAppName}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\{#MyAppId}_is1"; ValueType: string; ValueName: "DisplayVersion"; ValueData: "{#AppVersion}"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\{#MyAppId}_is1"; ValueType: string; ValueName: "Publisher"; ValueData: "PDFXI contributors"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\{#MyAppId}_is1"; ValueType: string; ValueName: "DisplayIcon"; ValueData: "{app}\pdfxi-ui.exe"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\{#MyAppId}_is1"; ValueType: dword; ValueName: "NoModify"; ValueData: "1"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\{#MyAppId}_is1"; ValueType: dword; ValueName: "NoRepair"; ValueData: "1"
; "Open with" list entry: registered as an application, not as the .pdf handler.
Root: HKCU; Subkey: "Software\Classes\Applications\pdfxi-ui.exe\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\pdfxi-ui.exe"" ""%1"""; Flags: uninsdeletekey; Tasks: openwith
Root: HKCU; Subkey: "Software\Classes\Applications\pdfxi-ui.exe\SupportedTypes"; ValueType: string; ValueName: ".pdf"; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKCU; Subkey: "Software\Classes\.pdf\OpenWithList\pdfxi-ui.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith

[UninstallDelete]
; Remove the installation directory itself (Inno otherwise leaves an empty
; directory). Files are already removed by the uninstall log; user settings in
; {userappdata}\PDFXI are never referenced here.
Type: filesandordirs; Name: "{app}"

[Run]
Filename: "{app}\pdfxi-ui.exe"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent