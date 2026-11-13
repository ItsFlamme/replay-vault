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
; be removed. Only this copy: a portable one elsewhere keeps running. First it's asked to quit
; the normal way, so a full recording is finished and saved (--quit waits up to 30 s for that);
; then whatever is left of it is stopped by force. The path goes to PowerShell quoted by
; PsQuotedExe, so a quote in it (an apostrophe in the user name) can't break the command.
Filename: "{app}\replay-vault.exe"; Parameters: "--quit"; Flags: runhidden waituntilterminated; RunOnceId: "QuitApp"
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -Command ""Get-Process replay-vault -ErrorAction SilentlyContinue | Where-Object Path -eq {code:PsQuotedExe} | Stop-Process -Force"""; Flags: runhidden; RunOnceId: "StopApp"

[InstallDelete]
; An update clears the old copy's leftovers for good (files are replaced in place, never recycled).
Type: files; Name: "{app}\debug.log"

[UninstallDelete]
; Chromium (in OBS's browser, or an overlay injected into the app) leaves a debug.log in the
; app's folder; without this the folder stays behind with just that file in it.
Type: files; Name: "{app}\debug.log"

[Code]
// The installed exe's path as a PowerShell single-quoted string. In one, a quote is written
// twice; PowerShell also takes the curly quotes for one, so those are doubled too.
function PsQuotedExe(Param: String): String;
var
  S: String;
begin
  S := ExpandConstant('{app}\replay-vault.exe');
  StringChangeEx(S, '''', '''''', True);
  StringChangeEx(S, #$2018, #$2018#$2018, True);
  StringChangeEx(S, #$2019, #$2019#$2019, True);
  StringChangeEx(S, #$201A, #$201A#$201A, True);
  StringChangeEx(S, #$201B, #$201B#$201B, True);
  Result := '''' + S + '''';
end;

[Messages]
ConfirmUninstall=Remove Replay Vault?%n%nYour clips and settings stay. To also undo what the app changed on this PC (its private OBS, firewall rule, GPU and Game Bar settings), cancel and use General > Remove from this PC instead (it uninstalls the app too).
