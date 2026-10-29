; РЈСЃС‚Р°РЅРѕРІС‰РёРє OmniFlex (Inno Setup 6). РЎР±РѕСЂРєР°: cargo build --release, Р·Р°С‚РµРј ISCC installer\OmniFlex.iss

#define AppName "OmniFlex"
#ifndef AppVersion
  #define AppVersion "1.3.2"
#endif
#define AppExe "OmniFlex.exe"

[Setup]
AppId={{3B7E9C14-6A2D-4F8B-A1C5-0E9D7F2B4A61}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppName}
VersionInfoVersion={#AppVersion}
; РЈСЃС‚Р°РЅРѕРІРєР° РґР»СЏ С‚РµРєСѓС‰РµРіРѕ РїРѕР»СЊР·РѕРІР°С‚РµР»СЏ вЂ” Р±РµР· Р·Р°РїСЂРѕСЃР° РїСЂР°РІ Р°РґРјРёРЅРёСЃС‚СЂР°С‚РѕСЂР°
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
OutputDir=..\dist
OutputBaseFilename=OmniFlex-Setup-{#AppVersion}
SetupIconFile=..\res\app.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
CloseApplications=no

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
russian.AutostartTask=Р—Р°РїСѓСЃРєР°С‚СЊ OmniFlex РІРјРµСЃС‚Рµ СЃ Windows
english.AutostartTask=Start OmniFlex with Windows
russian.ExtraGroup=Р”РѕРїРѕР»РЅРёС‚РµР»СЊРЅРѕ:
english.ExtraGroup=Additional options:
russian.RemoveSettings=РЈРґР°Р»РёС‚СЊ С‚Р°РєР¶Рµ РЅР°СЃС‚СЂРѕР№РєРё OmniFlex (РіРѕСЂСЏС‡РёРµ РєР»Р°РІРёС€Рё, С‚РµРјР°, СЃРІРѕРё С‚РµРєСЃС‚С‹ РїРµСЃРµРЅ)?
english.RemoveSettings=Also remove OmniFlex settings (hotkeys, theme, your lyrics)?

[Tasks]
Name: "autostart"; Description: "{cm:AutostartTask}"; GroupDescription: "{cm:ExtraGroup}"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; РўРѕС‚ Р¶Рµ РєР»СЋС‡, С‡С‚Рѕ РїРµСЂРµРєР»СЋС‡Р°РµС‚СЃСЏ РІ РЅР°СЃС‚СЂРѕР№РєР°С… РїСЂРѕРіСЂР°РјРјС‹
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "OmniFlex"; ValueData: """{app}\{#AppExe}"" --tray"; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
procedure KillApp();
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  KillApp();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  KillApp();
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    // РђРІС‚РѕР·Р°РїСѓСЃРє РјРѕРі Р±С‹С‚СЊ РІРєР»СЋС‡С‘РЅ РёР· СЃР°РјРѕР№ РїСЂРѕРіСЂР°РјРјС‹, Р° РЅРµ СѓСЃС‚Р°РЅРѕРІС‰РёРєРѕРј
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'OmniFlex');
    if (not UninstallSilent) and
       (MsgBox(CustomMessage('RemoveSettings'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES) then
      DelTree(ExpandConstant('{userappdata}\OmniFlex'), True, True, True);
  end;
end;
