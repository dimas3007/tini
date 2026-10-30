; Per-user installer for tini (Inno Setup 6). No admin rights needed.
;   iscc /DAppVersion=0.1.0 /DBinDir=..\target\release installer\tini.iss
; Installs to %LOCALAPPDATA%\Programs\tini. The path is fixed so Claude Code hooks, the MCP server
; registration and the Run key keep pointing at the same exe across updates.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef BinDir
  #define BinDir "..\target\release"
#endif

[Setup]
AppId={{7D5BE64C-3C73-4228-9B78-4C0B1FA90F06}
AppName=tini
AppVersion={#AppVersion}
AppVerName=tini {#AppVersion}
AppPublisher=tini contributors
AppPublisherURL=https://github.com/dimas3007/tini
AppSupportURL=https://github.com/dimas3007/tini/issues
AppUpdatesURL=https://github.com/dimas3007/tini/releases
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\tini
DisableDirPage=yes
DefaultGroupName=tini
DisableProgramGroupPage=yes
DisableReadyPage=yes
UninstallDisplayName=tini
UninstallDisplayIcon={app}\notch.exe
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
; Closing notch is done in [Code] (only the copy in {app}); Restart Manager is not needed.
CloseApplications=no
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir=..\dist
OutputBaseFilename=tini-setup
SetupLogging=yes

[Tasks]
Name: autostart; Description: "Start tini when I sign in to Windows"

[Files]
Source: "{#BinDir}\notch.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\notch-bridge.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\adhan.mp3"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\tini"; Filename: "{app}\notch.exe"; Comment: "A tiny dynamic island for Windows"

[Registry]
; Same value notch.exe itself writes for "Start with Windows" (autostart.rs).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Notch"; ValueData: """{app}\notch.exe"""; Tasks: autostart; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Notch"; Flags: deletevalue uninsdeletevalue; Tasks: not autostart

[Run]
; Agents that are already connected (e.g. from a portable copy): point them at this copy of
; notch-bridge.exe. Nothing is connected here that wasn't before; new connections come from Settings.
Filename: "{app}\notch-bridge.exe"; Parameters: "install"; Flags: runhidden waituntilterminated; Check: ClaudeHooksInstalled
Filename: "{cmd}"; Parameters: "/c claude mcp remove notch -s user & claude mcp add --scope user notch -- ""{app}\notch-bridge.exe"" mcp"; Flags: runhidden waituntilterminated; Check: ClaudeCodeMcpConnected
Filename: "{app}\notch-bridge.exe"; Parameters: "install-mcp claude-app"; Flags: runhidden waituntilterminated; Check: ClaudeAppConnected
Filename: "{app}\notch-bridge.exe"; Parameters: "install-mcp codex"; Flags: runhidden waituntilterminated; Check: CodexConnected
Filename: "{app}\notch-bridge.exe"; Parameters: "install-mcp antigravity"; Flags: runhidden waituntilterminated; Check: AntigravityConnected
Filename: "{app}\notch-bridge.exe"; Parameters: "install-mcp vscode"; Flags: runhidden waituntilterminated; Check: VsCodeConnected
Filename: "{app}\notch.exe"; Description: "Start tini now"; Flags: nowait postinstall

[UninstallDelete]
Type: files; Name: "{app}\*.log"

[Code]
// Stops notch.exe and every notch-bridge.exe started from this install folder (not dev builds
// elsewhere). Agents keep `notch-bridge mcp` running for their whole session, which locks the exe;
// they start it again on their next session.
procedure StopNotch();
var
  Dir, Cmd: String;
  Code: Integer;
begin
  Dir := ExpandConstant('{app}');
  StringChangeEx(Dir, '''', '''''', True);
  Cmd := '-NoProfile -ExecutionPolicy Bypass -Command "Get-Process notch, notch-bridge -ErrorAction SilentlyContinue | ' +
    'Where-Object { $_.Path -and ([IO.Path]::GetDirectoryName($_.Path) -eq ''' + Dir + ''') } | ' +
    'ForEach-Object { $_.Kill(); $_.WaitForExit(5000) | Out-Null }"';
  Exec('powershell.exe', Cmd, '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

// The agent configs below are only touched when they already point at notch-bridge, so the
// installer and uninstaller never create config files for agents the user didn't connect.
function FileHasNotch(const Path: String): Boolean;
var
  Text: AnsiString;
begin
  Result := LoadStringFromFile(Path, Text) and (Pos('notch-bridge', Text) > 0);
end;

function HomeDir(): String;
begin
  Result := ExpandConstant('{%USERPROFILE}');
end;

function ClaudeHooksInstalled(): Boolean;
begin
  Result := FileHasNotch(HomeDir() + '\.claude\settings.json');
end;

function ClaudeCodeMcpConnected(): Boolean;
begin
  Result := FileHasNotch(HomeDir() + '\.claude.json');
end;

// Any claude_desktop_config.json of the Claude app (Store or classic install) that lists notch.
function ClaudeAppConnected(): Boolean;
var
  Found: TFindRec;
  Base: String;
begin
  Result := FileHasNotch(ExpandConstant('{userappdata}\Claude\claude_desktop_config.json'));
  Base := ExpandConstant('{localappdata}\Claude-Data\');
  if not Result and FindFirst(Base + 'claude_*', Found) then
  try
    repeat
      Result := FileHasNotch(Base + Found.Name + '\claude_desktop_config.json');
    until Result or not FindNext(Found);
  finally
    FindClose(Found);
  end;
  Base := ExpandConstant('{localappdata}\Packages\');
  if not Result and FindFirst(Base + 'Claude_*', Found) then
  try
    repeat
      Result := FileHasNotch(Base + Found.Name + '\LocalCache\Roaming\Claude\claude_desktop_config.json');
    until Result or not FindNext(Found);
  finally
    FindClose(Found);
  end;
end;

function CodexConnected(): Boolean;
var
  Dir: String;
begin
  Dir := GetEnv('CODEX_HOME');
  if Dir = '' then
    Dir := HomeDir() + '\.codex';
  Result := FileHasNotch(Dir + '\config.toml');
end;

function AntigravityConnected(): Boolean;
begin
  Result := FileHasNotch(HomeDir() + '\.gemini\antigravity\mcp_config.json');
end;

// User-level VS Code MCP config ("MCP: Open User Configuration").
function VsCodeConnected(): Boolean;
begin
  Result := FileHasNotch(ExpandConstant('{userappdata}\Code\User\mcp.json'));
end;

procedure RunHidden(const FileName, Params: String);
var
  Code: Integer;
begin
  Exec(FileName, Params, '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

// Removes every hook and MCP entry that points at this install before its files are deleted.
procedure DisconnectAgents();
var
  Bridge: String;
begin
  Bridge := ExpandConstant('{app}\notch-bridge.exe');
  if not FileExists(Bridge) then
    Exit;
  if ClaudeHooksInstalled() then
    RunHidden(Bridge, 'uninstall');
  if ClaudeCodeMcpConnected() then
    RunHidden(ExpandConstant('{cmd}'), '/c claude mcp remove notch -s user');
  if ClaudeAppConnected() then
    RunHidden(Bridge, 'uninstall-mcp claude-app');
  if CodexConnected() then
    RunHidden(Bridge, 'uninstall-mcp codex');
  if AntigravityConnected() then
    RunHidden(Bridge, 'uninstall-mcp antigravity');
  if VsCodeConnected() then
    RunHidden(Bridge, 'uninstall-mcp vscode');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopNotch();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopNotch();
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Data: String;
begin
  // usUninstall runs before any file is deleted, so notch-bridge.exe is still there.
  if CurUninstallStep = usUninstall then
    DisconnectAgents();
  if CurUninstallStep <> usPostUninstall then
    Exit;
  Data := ExpandConstant('{userappdata}\Notch');
  if DirExists(Data) and not UninstallSilent() then
    if MsgBox('Also delete your tasks, Deck buttons and settings?' + #13#10#13#10 + Data,
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      DelTree(Data, True, True, True);
end;
