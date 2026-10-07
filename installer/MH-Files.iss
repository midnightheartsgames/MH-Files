; Установщик MH Files (Inno Setup 6). Собирается из tools\build-release.ps1:
;   iscc /DAppVersion=1.0.0-rc.1 /DFileVersion=1.0.0 /DSourceDir=..\target\dist\app installer\MH-Files.iss
;
; Ставится для текущего пользователя, без прав администратора: в
; %LOCALAPPDATA%\Programs\MH Files. Настройки (%APPDATA%\MH Files) при удалении остаются —
; их удаляет только сам пользователь.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
; Версия файла — только числа: из 1.0.0-rc.1 берётся 1.0.0.
#ifndef FileVersion
  #define FileVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\dist\app"
#endif

[Setup]
AppId={{3B6E2B7C-6F7B-4E9A-9C8D-5D1F3A2B7E41}
AppName=MH Files
AppVersion={#AppVersion}
AppVerName=MH Files {#AppVersion}
VersionInfoVersion={#FileVersion}
VersionInfoProductVersion={#FileVersion}
VersionInfoProductTextVersion={#AppVersion}
AppPublisher=midnightheartsgames
AppPublisherURL=https://github.com/midnightheartsgames/MH-Files
DefaultDirName={localappdata}\Programs\MH Files
DefaultGroupName=MH Files
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\target\dist
OutputBaseFilename=MH-Files-{#AppVersion}-setup
SetupIconFile=..\assets\icon\MH-Files.ico
UninstallDisplayIcon={app}\MH-Files.exe
UninstallDisplayName=MH Files
LicenseFile=..\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Работающую копию программа закрывает сама перед обновлением.
CloseApplications=yes
RestartApplications=yes
; Подпись: build-release.ps1 передаёт /DSign и команду mhsign (signtool с сертификатом).
#ifdef Sign
SignTool=mhsign
SignedUninstaller=yes
#endif

[Languages]
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Значок на рабочем столе"; Flags: unchecked
Name: "explorermenu"; Description: "Пункт «Открыть в MH Files» в меню Проводника"
Name: "archives"; Description: "MH Files в «Открыть с помощью» для архивов zip, 7z, rar"; Flags: unchecked

[Files]
Source: "{#SourceDir}\MH-Files.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\assets\fonts\OFL.txt"; DestDir: "{app}"; DestName: "OFL-Cuprum.txt"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\MH Files"; Filename: "{app}\MH-Files.exe"
Name: "{userdesktop}\MH Files"; Filename: "{app}\MH-Files.exe"; Tasks: desktopicon

; Те же ключи, что пишет «Настройки → Система → Меню Проводника» (platform::integration):
; удаление программы убирает их в любом случае.
[Registry]
Root: HKCU; Subkey: "Software\Classes\Directory\shell\MHFiles"; ValueType: string; ValueData: "Открыть в MH Files"; Flags: uninsdeletekey; Tasks: explorermenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\MHFiles"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\MH-Files.exe"",0"; Tasks: explorermenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\MHFiles\command"; ValueType: string; ValueData: """{app}\MH-Files.exe"" ""%V"""; Tasks: explorermenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\MHFiles"; ValueType: string; ValueData: "Открыть в MH Files"; Flags: uninsdeletekey; Tasks: explorermenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\MHFiles"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\MH-Files.exe"",0"; Tasks: explorermenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\MHFiles\command"; ValueType: string; ValueData: """{app}\MH-Files.exe"" ""%V"""; Tasks: explorermenu
Root: HKCU; Subkey: "Software\Classes\Drive\shell\MHFiles"; ValueType: string; ValueData: "Открыть в MH Files"; Flags: uninsdeletekey; Tasks: explorermenu
Root: HKCU; Subkey: "Software\Classes\Drive\shell\MHFiles"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\MH-Files.exe"",0"; Tasks: explorermenu
Root: HKCU; Subkey: "Software\Classes\Drive\shell\MHFiles\command"; ValueType: string; ValueData: """{app}\MH-Files.exe"" ""%V"""; Tasks: explorermenu

; Архивы: ProgID и его имя в OpenWithProgids — как «Настройки → Система → Архивы».
Root: HKCU; Subkey: "Software\Classes\MHFiles.Archive"; ValueType: string; ValueData: "Архив"; Flags: uninsdeletekey; Tasks: archives
Root: HKCU; Subkey: "Software\Classes\MHFiles.Archive\DefaultIcon"; ValueType: string; ValueData: """{app}\MH-Files.exe"",0"; Tasks: archives
Root: HKCU; Subkey: "Software\Classes\MHFiles.Archive\shell\open"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "MH Files"; Tasks: archives
Root: HKCU; Subkey: "Software\Classes\MHFiles.Archive\shell\open\command"; ValueType: string; ValueData: """{app}\MH-Files.exe"" ""%1"""; Tasks: archives
Root: HKCU; Subkey: "Software\Classes\.zip\OpenWithProgids"; ValueType: none; ValueName: "MHFiles.Archive"; Flags: uninsdeletevalue; Tasks: archives
Root: HKCU; Subkey: "Software\Classes\.7z\OpenWithProgids"; ValueType: none; ValueName: "MHFiles.Archive"; Flags: uninsdeletevalue; Tasks: archives
Root: HKCU; Subkey: "Software\Classes\.rar\OpenWithProgids"; ValueType: none; ValueName: "MHFiles.Archive"; Flags: uninsdeletevalue; Tasks: archives

; Пункт меню, папки по умолчанию и архивы мог включить и сам пользователь из настроек (без
; задачи установщика) — при удалении программы всё это убирается в любом случае.
[Code]
// Двойной щелчок по папкам возвращается Проводнику, только если там по-прежнему MH Files.
procedure RestoreDefaultVerb(const Key: String);
var
  Verb: String;
begin
  if RegQueryStringValue(HKCU, Key, '', Verb) and (Verb = 'MHFiles') then
    RegDeleteValue(HKCU, Key, '');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RestoreDefaultVerb('Software\Classes\Directory\shell');
    RestoreDefaultVerb('Software\Classes\Drive\shell');
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\MHFiles.Archive');
    RegDeleteValue(HKCU, 'Software\Classes\.zip\OpenWithProgids', 'MHFiles.Archive');
    RegDeleteValue(HKCU, 'Software\Classes\.7z\OpenWithProgids', 'MHFiles.Archive');
    RegDeleteValue(HKCU, 'Software\Classes\.rar\OpenWithProgids', 'MHFiles.Archive');
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\Directory\shell\MHFiles');
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\Directory\Background\shell\MHFiles');
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\Drive\shell\MHFiles');
  end;
end;

[Run]
Filename: "{app}\MH-Files.exe"; Description: "Запустить MH Files"; Flags: nowait postinstall skipifsilent
