#ifndef AppVersion
  #error AppVersion must be provided with /DAppVersion
#endif
#ifndef StageDir
  #error StageDir must be provided with /DStageDir
#endif
#ifndef OutputDir
  #error OutputDir must be provided with /DOutputDir
#endif

[Setup]
AppId={{80F98D61-E5DA-4C99-9361-B5265C53FE31}
AppName=Nemuri
AppVersion={#AppVersion}
AppPublisher=HZ-TYZQ
AppPublisherURL=https://github.com/HZ-TYZQ/nemuri
AppSupportURL=https://github.com/HZ-TYZQ/nemuri/issues
AppUpdatesURL=https://github.com/HZ-TYZQ/nemuri/releases
DefaultDirName={localappdata}\Programs\Nemuri
DefaultGroupName=Nemuri
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline
ArchitecturesAllowed=x64compatible and not arm64
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
SetupIconFile=..\..\assets\icons\nemuri.ico
UninstallDisplayIcon={app}\nemuri.ico
LicenseFile=..\..\LICENSE
OutputDir={#OutputDir}
OutputBaseFilename=nemuri-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
; ChineseSimplified is a user-contributed translation that Inno Setup does
; not bundle with the compiler. It is vendored next to this script, pinned
; to the issrc "is-6_7_1" tree matching the Inno Setup version used by CI.
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"

[CustomMessages]
english.AddToPath=Add Nemuri to the current user PATH
chinesesimplified.AddToPath=将 Nemuri 添加到当前用户 PATH

[Tasks]
Name: "addtopath"; Description: "{cm:AddToPath}"; Flags: unchecked

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

; 1.4.0 and earlier were installed as Music Player under the same AppId.
; Upgrading in place keeps the old install folder (UsePreviousAppDir), so the
; files and the Start menu entry with the old names have to be removed here.
[InstallDelete]
Type: files; Name: "{app}\music-player.exe"
Type: files; Name: "{app}\music-player.cmd"
Type: files; Name: "{app}\music-player.ico"
Type: files; Name: "{autoprograms}\Music Player.lnk"

[Icons]
Name: "{autoprograms}\Nemuri"; Filename: "{app}\nemuri.exe"; WorkingDir: "{app}"; IconFilename: "{app}\nemuri.ico"

[Code]
const
  // Keeps the name from before the rename: an install upgraded from 1.4.0
  // recorded its PATH flag here, and its uninstaller must still find it.
  PathFlagKey = 'Software\HZ-TYZQ\Music Player';

function NormalizePath(Value: String): String;
begin
  Result := Lowercase(Trim(Value));
  if (Length(Result) >= 2) and (Result[1] = '"') and
     (Result[Length(Result)] = '"') then
  begin
    Delete(Result, Length(Result), 1);
    Delete(Result, 1, 1);
  end;
  StringChangeEx(Result, '/', '\', True);
  while (Length(Result) > 3) and
        ((Result[Length(Result)] = '\') or (Result[Length(Result)] = '/')) do
    Delete(Result, Length(Result), 1);
end;

function PathContains(CurrentPath, Directory: String): Boolean;
var
  Rest, Entry: String;
  SeparatorPos: Integer;
  Target: String;
begin
  Result := False;
  Target := NormalizePath(Directory);
  Rest := CurrentPath;
  while Length(Rest) > 0 do
  begin
    SeparatorPos := Pos(';', Rest);
    if SeparatorPos > 0 then
    begin
      Entry := Copy(Rest, 1, SeparatorPos - 1);
      Delete(Rest, 1, SeparatorPos);
    end
    else
    begin
      Entry := Rest;
      Rest := '';
    end;
    if NormalizePath(Entry) = Target then
    begin
      Result := True;
      Exit;
    end;
  end;
end;

function AddToUserPath(Directory: String): Boolean;
var
  CurrentPath: String;
begin
  Result := False;
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', CurrentPath) then
    CurrentPath := '';
  if PathContains(CurrentPath, Directory) then
    Exit;
  if (CurrentPath <> '') and (CurrentPath[Length(CurrentPath)] <> ';') then
    CurrentPath := CurrentPath + ';';
  if not RegWriteExpandStringValue(HKCU, 'Environment', 'Path', CurrentPath + Directory) then
    RaiseException('Unable to update the current user PATH');
  Result := True;
end;

procedure RemoveFromUserPath(Directory: String);
var
  CurrentPath, NewPath, Entry, Rest, Target: String;
  SeparatorPos: Integer;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', CurrentPath) then
    Exit;
  Target := NormalizePath(Directory);
  NewPath := '';
  Rest := CurrentPath;
  while Length(Rest) > 0 do
  begin
    SeparatorPos := Pos(';', Rest);
    if SeparatorPos > 0 then
    begin
      Entry := Copy(Rest, 1, SeparatorPos - 1);
      Delete(Rest, 1, SeparatorPos);
    end
    else
    begin
      Entry := Rest;
      Rest := '';
    end;
    Entry := Trim(Entry);
    if (Entry <> '') and (NormalizePath(Entry) <> Target) then
    begin
      if NewPath <> '' then
        NewPath := NewPath + ';';
      NewPath := NewPath + Entry;
    end;
  end;
  if NewPath = '' then
    RegDeleteValue(HKCU, 'Environment', 'Path')
  else if not RegWriteExpandStringValue(HKCU, 'Environment', 'Path', NewPath) then
    RaiseException('Unable to clean the current user PATH');
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') and
     AddToUserPath(ExpandConstant('{app}')) then
    RegWriteDWordValue(HKCU, PathFlagKey,
      'PathAddedByInstaller', 1);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  PathAddedByInstaller: Cardinal;
begin
  if (CurUninstallStep = usUninstall) and
     RegQueryDWordValue(HKCU, PathFlagKey,
       'PathAddedByInstaller', PathAddedByInstaller) and
     (PathAddedByInstaller = 1) then
  begin
    RemoveFromUserPath(ExpandConstant('{app}'));
    RegDeleteValue(HKCU, PathFlagKey,
      'PathAddedByInstaller');
    RegDeleteKeyIfEmpty(HKCU, PathFlagKey);
    RegDeleteKeyIfEmpty(HKCU, 'Software\HZ-TYZQ');
  end;
end;
