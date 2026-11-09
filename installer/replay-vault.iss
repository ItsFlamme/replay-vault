; Inno Setup 6 script for Replay Vault. Built by tools/release.ps1, which passes
; /DAppVersion=x.y.z /DSrcDir=<folder with the exe and notices> /DOutDir=<dist folder>.
; Installs per user (no admin prompt) into %LOCALAPPDATA%\Programs\Replay Vault.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif

[Setup]
AppId={{4526C432-E5F8-4F45-9627-90D46ED761D7}
AppName=Replay Vault
AppVersion={#AppVersion}
AppVerName=Replay Vault {#AppVersion}
AppPublisher=ItsFlamme
AppPublisherURL=https://replayvault.app
AppSupportURL=https://github.com/ItsFlamme/replay-vault/issues
AppUpdatesURL=https://github.com/ItsFlamme/replay-vault/releases
DefaultDirName={autopf}\Replay Vault
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutDir}
OutputBaseFilename=ReplayVault-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\assets\replay-vault.ico
UninstallDisplayIcon={app}\replay-vault.exe
UninstallDisplayName=Replay Vault
; Closes a running copy (and so its hidden OBS) before files are replaced.
CloseApplications=force
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SrcDir}\replay-vault.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\NOTICE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Replay Vault"; Filename: "{app}\replay-vault.exe"
Name: "{autodesktop}\Replay Vault"; Filename: "{app}\replay-vault.exe"; Tasks: desktopicon

[Registry]
; "Start with Windows" lives in the user's Run list; take it out on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Replay Vault"; Flags: uninsdeletevalue dontcreatekey

[Run]
Filename: "{app}\replay-vault.exe"; Description: "{cm:LaunchProgram,Replay Vault}"; Flags: nowait postinstall skipifsilent
; An update from the app's Install now runs this silently; open the new version when it's done.
Filename: "{app}\replay-vault.exe"; Flags: nowait; Check: WizardSilent

[UninstallRun]
; The app lives in the tray; stop this installed copy (its OBS goes with it) so its files can
; be removed. Only this copy: a portable one elsewhere keeps running.
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -Command ""Get-Process replay-vault -ErrorAction SilentlyContinue | Where-Object Path -eq '{app}\replay-vault.exe' | Stop-Process -Force"""; Flags: runhidden; RunOnceId: "StopApp"

[Messages]
ConfirmUninstall=Remove Replay Vault?%n%nYour clips and settings stay. To also undo what the app changed on this PC (its private OBS, firewall rule, GPU and Game Bar settings), cancel and use General > Remove from this PC instead (it uninstalls the app too).
