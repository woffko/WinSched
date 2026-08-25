#define AppName "WinSched"
#ifndef AppVersion
  #define AppVersion "0.4.0"
#endif

#ifndef PayloadDir
  #error PayloadDir must point to the frozen WinSched payload directory
#endif
#ifndef ProjectRoot
  #error ProjectRoot must point to the WinSched source root
#endif
#ifndef OutputDir
  #define OutputDir ProjectRoot + "\dist\gui-installer"
#endif

[Setup]
AppId={{4E3F986C-6D8F-4E4D-97AF-33A3024DF83B}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=WinSched Project
VersionInfoVersion={#AppVersion}.0
VersionInfoProductName={#AppName}
VersionInfoDescription=WinSched Windows 11 CPU placement controller
DefaultDirName={autopf}\WinSched
DefaultGroupName=WinSched
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\winsched-tray.exe
OutputDir={#OutputDir}
OutputBaseFilename=WinSched-{#AppVersion}-Setup-x64
SetupIconFile={#ProjectRoot}\assets\tray\winsched.ico
LicenseFile={#ProjectRoot}\LICENSE
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern dynamic windows11
WizardImageFile={#ProjectRoot}\assets\installer\winsched-wizard.png
WizardImageFileDynamicDark={#ProjectRoot}\assets\installer\winsched-wizard-dark.png
WizardSmallImageFile={#ProjectRoot}\assets\installer\winsched-wizard-small.png
WizardSmallImageFileDynamicDark={#ProjectRoot}\assets\installer\winsched-wizard-small-dark.png
DisableWelcomePage=no
DisableProgramGroupPage=auto
PrivilegesRequired=admin
SetupArchitecture=x64
ArchitecturesAllowed=x64os
MinVersion=10.0.22000
CloseApplications=yes
CloseApplicationsFilter=winsched-tray.exe,winsched-settings.exe
RestartApplications=no
UsePreviousAppDir=yes
UsePreviousTasks=yes
UninstallLogMode=append
SetupMutex=Global\WinSched.Setup
SetupLogging=yes
Uninstallable=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[CustomMessages]
english.StartupTask=Start the WinSched tray automatically when a user signs in
russian.StartupTask=Автоматически запускать WinSched в области уведомлений при входе пользователя
english.DesktopTask=Create a desktop shortcut
russian.DesktopTask=Создать ярлык на рабочем столе
english.ServiceInstallFailed=The WinSched service could not be registered or started. Setup cannot continue.
russian.ServiceInstallFailed=Не удалось зарегистрировать или запустить службу WinSched. Установка не может быть продолжена.
english.ServiceRemoveFailed=The existing WinSched service could not be stopped and removed.
russian.ServiceRemoveFailed=Не удалось остановить и удалить существующую службу WinSched.
english.PurgeDataPrompt=Also remove the WinSched configuration, logs, and saved state?%n%nThe default is No.
russian.PurgeDataPrompt=Также удалить конфигурацию, журналы и сохранённое состояние WinSched?%n%nПо умолчанию выбран ответ «Нет».

[Tasks]
Name: "startup"; Description: "{cm:StartupTask}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "desktopicon"; Description: "{cm:DesktopTask}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Dirs]
Name: "{commonappdata}\WinSched"; Flags: uninsneveruninstall

[InstallDelete]
Type: files; Name: "{group}\WinSched Tray.lnk"
Type: files; Name: "{group}\WinSched Configuration.lnk"

[Files]
Source: "{#PayloadDir}\winsched.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\winsched-service.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\winsched-tray.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\winsched-settings.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\winsched.toml"; DestDir: "{commonappdata}\WinSched"; DestName: "winsched.toml"; Flags: onlyifdoesntexist uninsneveruninstall

[Registry]
Root: HKLM64; Subkey: "Software\WinSched"; ValueType: string; ValueName: "InstallPath"; ValueData: "{app}"; Flags: uninsdeletekeyifempty uninsdeletevalue; AfterInstall: ProvisionService

[Icons]
Name: "{group}\WinSched"; Filename: "{app}\winsched-tray.exe"; WorkingDir: "{app}"
Name: "{group}\WinSched Settings"; Filename: "{app}\winsched-settings.exe"; WorkingDir: "{app}"
Name: "{group}\WinSched Configuration (Advanced)"; Filename: "{sys}\notepad.exe"; Parameters: """{commonappdata}\WinSched\winsched.toml"""
Name: "{group}\WinSched Logs"; Filename: "{sys}\notepad.exe"; Parameters: """{commonappdata}\WinSched\winsched.log"""
Name: "{group}\Uninstall WinSched"; Filename: "{uninstallexe}"
Name: "{commondesktop}\WinSched"; Filename: "{app}\winsched-tray.exe"; WorkingDir: "{app}"; Tasks: desktopicon
Name: "{commonstartup}\WinSched Tray"; Filename: "{app}\winsched-tray.exe"; WorkingDir: "{app}"; Tasks: startup

[Run]
Filename: "{app}\winsched-tray.exe"; Description: "{cm:LaunchProgram,WinSched}"; WorkingDir: "{app}"; Flags: postinstall nowait skipifsilent runasoriginaluser

[UninstallDelete]
Type: files; Name: "{group}\WinSched Tray.lnk"
Type: files; Name: "{group}\WinSched Configuration.lnk"
Type: dirifempty; Name: "{app}"
Type: dirifempty; Name: "{group}"

[Code]
const
  ServiceName = 'WinSched';

var
  ExistingService: Boolean;
  ServiceStoppedBySetup: Boolean;
  ProvisionSucceeded: Boolean;
  PurgeDataRequested: Boolean;

function ExecQuiet(const FileName, Parameters: String; var ResultCode: Integer): Boolean;
begin
  Result := Exec(FileName, Parameters, '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

function ServiceExists: Boolean;
var
  ResultCode: Integer;
begin
  Result :=
    ExecQuiet(ExpandConstant('{sys}\sc.exe'), 'query ' + ServiceName, ResultCode) and
    (ResultCode = 0);
end;

function ServiceExecutable: String;
begin
  Result := ExpandConstant('{app}\winsched-service.exe');
  if not FileExists(Result) then
    Result := ExpandConstant('{commonappdata}\WinSched\winsched-service.exe');
end;

function ServiceIsStopped: Boolean;
var
  ResultCode: Integer;
  Parameters: String;
begin
  Parameters :=
    '-NoProfile -NonInteractive -Command "' +
    '$service = Get-Service -Name ''' + ServiceName +
    ''' -ErrorAction SilentlyContinue; ' +
    'if (($null -eq $service) -or ($service.Status -eq ''Stopped'')) ' +
    '{ exit 0 } else { exit 1 }"';
  Result :=
    ExecQuiet(
      ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
      Parameters,
      ResultCode) and
    (ResultCode = 0);
end;

procedure StopTray;
var
  ResultCode: Integer;
begin
  ExecQuiet(
    ExpandConstant('{sys}\taskkill.exe'),
    '/F /IM winsched-tray.exe',
    ResultCode);
  ExecQuiet(
    ExpandConstant('{sys}\taskkill.exe'),
    '/F /IM winsched-settings.exe',
    ResultCode);
  Sleep(1000);
end;

function StopExistingService: Boolean;
var
  ServiceExe: String;
  ResultCode: Integer;
  Attempt: Integer;
begin
  Result := True;
  if not ServiceExists then
    exit;

  ServiceExe := ServiceExecutable;
  if FileExists(ServiceExe) then begin
    Result := ExecQuiet(ServiceExe, 'stop', ResultCode) and (ResultCode = 0);
    if Result then begin
      ServiceStoppedBySetup := True;
      exit;
    end;
  end;

  ExecQuiet(
    ExpandConstant('{sys}\sc.exe'),
    'stop ' + ServiceName,
    ResultCode);
  for Attempt := 1 to 80 do begin
    if ServiceIsStopped then begin
      ServiceStoppedBySetup := True;
      Result := True;
      exit;
    end;
    Sleep(250);
  end;
  Result := False;
end;

function UnregisterExistingService: Boolean;
var
  ServiceExe: String;
  ResultCode: Integer;
  Attempt: Integer;
begin
  Result := True;
  if not ServiceExists then
    exit;

  ServiceExe := ServiceExecutable;

  if FileExists(ServiceExe) then
    ExecQuiet(ServiceExe, 'uninstall', ResultCode);

  if ServiceExists then begin
    ExecQuiet(ExpandConstant('{sys}\sc.exe'), 'stop ' + ServiceName, ResultCode);
    Sleep(500);
    ExecQuiet(ExpandConstant('{sys}\sc.exe'), 'delete ' + ServiceName, ResultCode);
  end;

  for Attempt := 1 to 80 do begin
    if not ServiceExists then
      exit;
    Sleep(250);
  end;
  Result := False;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  NeedsRestart := False;
  StopTray;
  ExistingService := ServiceExists;
  if ExistingService and (not StopExistingService) then
    Result := CustomMessage('ServiceRemoveFailed');
end;

procedure ProvisionService;
var
  ResultCode: Integer;
  Parameters: String;
  ServiceExe: String;
  ConfigPath: String;
begin
  ServiceExe := ExpandConstant('{app}\winsched-service.exe');
  ConfigPath := ExpandConstant('{commonappdata}\WinSched\winsched.toml');
  Parameters :=
    'provision --config "' + ConfigPath + '" --allow-auto --start';
  if (not ExecQuiet(ServiceExe, Parameters, ResultCode)) or (ResultCode <> 0) then begin
    RaiseException(CustomMessage('ServiceInstallFailed'));
  end;
  ProvisionSucceeded := True;
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\winsched.exe'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\winsched-service.exe'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\winsched-tray.exe'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\winsched-settings.exe'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\uninstall.ps1'));
end;

function HasCommandLineSwitch(const Switch: String): Boolean;
var
  Index: Integer;
begin
  Result := False;
  for Index := 1 to ParamCount do
    if CompareText(ParamStr(Index), Switch) = 0 then begin
      Result := True;
      exit;
    end;
end;

procedure DeinitializeSetup;
var
  ServiceExe: String;
  ResultCode: Integer;
begin
  if ExistingService and ServiceStoppedBySetup and
     (not ProvisionSucceeded) then begin
    ServiceExe := ServiceExecutable;
    if FileExists(ServiceExe) then begin
      if (not ExecQuiet(ServiceExe, 'start', ResultCode)) or
         (ResultCode <> 0) then
        Log('WinSched rollback restart through the service executable failed');
    end else begin
      if (not ExecQuiet(
        ExpandConstant('{sys}\sc.exe'),
        'start ' + ServiceName,
        ResultCode)) or (ResultCode <> 0) then
        Log('WinSched rollback restart through sc.exe failed');
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then begin
    StopTray;
    if not UnregisterExistingService then
      RaiseException(CustomMessage('ServiceRemoveFailed'));
    PurgeDataRequested := HasCommandLineSwitch('/PURGEDATA');
    if (not UninstallSilent) and (not PurgeDataRequested) then
      PurgeDataRequested :=
        MsgBox(
          CustomMessage('PurgeDataPrompt'),
          mbConfirmation,
          MB_YESNO or MB_DEFBUTTON2) = IDYES;
  end;
  if (CurUninstallStep = usPostUninstall) and
     PurgeDataRequested then
    DelTree(ExpandConstant('{commonappdata}\WinSched'), True, True, True);
end;
