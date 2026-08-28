#define AppName "WinSched"
#ifndef AppVersion
  #define AppVersion "0.6.0"
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
DisableDirPage=yes
PrivilegesRequired=admin
SetupArchitecture=x64
ArchitecturesAllowed=x64os
MinVersion=10.0.22621
AllowUNCPath=no
AllowNetworkDrive=no
AllowRootDirectory=no
CloseApplications=yes
CloseApplicationsFilter=winsched-tray.exe,winsched-monitor.exe,winsched-settings.exe
RestartApplications=no
UsePreviousAppDir=no
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
english.DataDirectorySecurityFailed=Setup could not secure the WinSched data directory. Installation cannot continue.
russian.DataDirectorySecurityFailed=Не удалось защитить каталог данных WinSched. Установка не может быть продолжена.
english.InstallPathInvalid=WinSched must be installed in the protected Program Files\WinSched directory.
russian.InstallPathInvalid=WinSched должен быть установлен в защищённый каталог Program Files\WinSched.
english.LegacyInstallPathUnsupported=An older WinSched GUI installation uses a non-default directory. Uninstall that version first, preserve ProgramData, then run this Setup again.
russian.LegacyInstallPathUnsupported=Предыдущая GUI-версия WinSched установлена в нестандартный каталог. Сначала удалите её, сохранив ProgramData, затем снова запустите этот установщик.
english.UntrustedDataDirectory=Setup found an orphaned WinSched data directory without a trusted service or GUI-install record. Move or remove that directory after reviewing it, then retry.
russian.UntrustedDataDirectory=Обнаружен оставшийся каталог данных WinSched без доверенной службы или записи GUI-установки. Проверьте и переместите либо удалите этот каталог, затем повторите установку.
english.PurgeDataPrompt=Also remove the WinSched configuration, logs, and saved state?%n%nThe default is No.
russian.PurgeDataPrompt=Также удалить конфигурацию, журналы и сохранённое состояние WinSched?%n%nПо умолчанию выбран ответ «Нет».
english.PurgeDataFailed=WinSched was removed, but its data directory could not be fully purged.
russian.PurgeDataFailed=WinSched удалён, но каталог данных не удалось удалить полностью.

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
Source: "{#PayloadDir}\winsched-monitor.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\winsched-tray.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\winsched-settings.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\winsched.toml"; DestDir: "{commonappdata}\WinSched"; DestName: "winsched.toml"; Flags: onlyifdoesntexist uninsneveruninstall
Source: "{#PayloadDir}\secure-data.ps1"; Flags: dontcopy
Source: "{#PayloadDir}\winsched-service.exe"; DestName: "winsched-service-helper.exe"; Flags: dontcopy

[Registry]
Root: HKLM64; Subkey: "Software\WinSched"; ValueType: string; ValueName: "InstallPath"; ValueData: "{app}"

[Icons]
Name: "{group}\WinSched"; Filename: "{app}\winsched-tray.exe"; WorkingDir: "{app}"
Name: "{group}\WinSched Process Monitor"; Filename: "{app}\winsched-monitor.exe"; WorkingDir: "{app}"
Name: "{group}\WinSched Settings"; Filename: "{app}\winsched-settings.exe"; WorkingDir: "{app}"
Name: "{group}\WinSched Configuration (Advanced)"; Filename: "{sys}\notepad.exe"; Parameters: """{commonappdata}\WinSched\winsched.toml"""
Name: "{group}\WinSched Logs"; Filename: "{sys}\notepad.exe"; Parameters: """{commonappdata}\WinSched\winsched.log"""
Name: "{group}\Uninstall WinSched"; Filename: "{uninstallexe}"
Name: "{commondesktop}\WinSched"; Filename: "{app}\winsched-tray.exe"; WorkingDir: "{app}"; Tasks: desktopicon
Name: "{commonstartup}\WinSched Tray"; Filename: "{app}\winsched-tray.exe"; WorkingDir: "{app}"; Tasks: startup

[Run]
Filename: "{app}\winsched-service.exe"; Parameters: "provision --config ""{commonappdata}\WinSched\winsched.toml"" --allow-auto --start --result-file ""{commonappdata}\WinSched\provision-result.txt"""; Flags: runhidden logoutput; AfterInstall: VerifyProvisionService
Filename: "{app}\winsched-tray.exe"; Description: "{cm:LaunchProgram,WinSched}"; WorkingDir: "{app}"; Flags: postinstall nowait skipifsilent runasoriginaluser; Check: ProvisionWasSuccessful

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
  PriorServiceStateCaptured: Boolean;
  ServiceWasRunning: Boolean;
  ServiceStoppedBySetup: Boolean;
  ProvisionAttempted: Boolean;
  ProvisionSucceeded: Boolean;
  PurgeDataRequested: Boolean;

function ExecQuiet(const FileName, Parameters: String; var ResultCode: Integer): Boolean;
begin
  Result := Exec(FileName, Parameters, '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

function ProvisionWasSuccessful: Boolean;
begin
  Result := ProvisionSucceeded;
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
end;

function TrustedSetupServiceHelper(var HelperPath: String): Boolean;
begin
  Result := False;
  try
    ExtractTemporaryFile('winsched-service-helper.exe');
  except
    exit;
  end;
  HelperPath := ExpandConstant('{tmp}\winsched-service-helper.exe');
  Result := FileExists(HelperPath);
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
  ExecQuiet(
    ExpandConstant('{sys}\taskkill.exe'),
    '/F /IM winsched-monitor.exe',
    ResultCode);
  Sleep(1000);
end;

function StopExistingService: Boolean;
var
  ServiceHelper: String;
  ResultCode: Integer;
  CommandSucceeded: Boolean;
  ActuallyStopped: Boolean;
begin
  Result := True;
  if not ServiceExists then
    exit;
  if not TrustedSetupServiceHelper(ServiceHelper) then begin
    Result := False;
    exit;
  end;
  CommandSucceeded :=
    ExecQuiet(ServiceHelper, 'stop', ResultCode) and (ResultCode = 0);
  ActuallyStopped := ServiceIsStopped;
  if ActuallyStopped then begin
    ServiceStoppedBySetup := True;
  end;
  Result := CommandSucceeded and ActuallyStopped;
end;

function OwnershipJournalsExist: Boolean;
begin
  Result :=
    FileExists(ExpandConstant('{commonappdata}\WinSched\managed-state.json')) or
    FileExists(ExpandConstant('{commonappdata}\WinSched\managed-state.bak')) or
    FileExists(ExpandConstant('{commonappdata}\WinSched\background-state.json')) or
    FileExists(ExpandConstant('{commonappdata}\WinSched\background-state.bak'));
end;

function UnregisterExistingService: Boolean;
var
  ServiceExe: String;
  ResultCode: Integer;
  Attempt: Integer;
begin
  Result := True;
  ServiceExe := ServiceExecutable;

  if not ServiceExists then begin
    if FileExists(ServiceExe) then begin
      Result :=
        ExecQuiet(
          ServiceExe,
          'uninstall --data-directory "' +
            ExpandConstant('{commonappdata}\WinSched') + '"',
          ResultCode) and
        (ResultCode = 0);
    end else if OwnershipJournalsExist then begin
      Result := False;
    end;
    exit;
  end;

  if not FileExists(ServiceExe) then begin
    Result := False;
    exit;
  end;
  if FileExists(ServiceExe) then begin
    if (not ExecQuiet(
         ServiceExe,
         'uninstall --data-directory "' +
           ExpandConstant('{commonappdata}\WinSched') + '"',
         ResultCode)) or
       (ResultCode <> 0) then begin
      Result := False;
      exit;
    end;
  end;

  for Attempt := 1 to 80 do begin
    if not ServiceExists then
      exit;
    Sleep(250);
  end;
  Result := False;
end;

function RunSecurityScript(const Directory: String; ValidateControlFiles: Boolean): Boolean;
var
  ScriptPath: String;
  Parameters: String;
  ResultCode: Integer;
begin
  Result := False;
  try
    ExtractTemporaryFile('secure-data.ps1');
  except
    exit;
  end;
  ScriptPath := ExpandConstant('{tmp}\secure-data.ps1');
  Parameters :=
    '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' +
    ScriptPath + '" -Directory "' + Directory + '" -Harden';
  if ValidateControlFiles then
    Parameters := Parameters + ' -Purpose Data -ValidateControlFiles'
  else
    Parameters := Parameters + ' -Purpose Application';
  Result :=
    ExecQuiet(
      ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
      Parameters,
      ResultCode) and
    (ResultCode = 0);
end;

function HardenDataDirectory: Boolean;
begin
  Result :=
    RunSecurityScript(ExpandConstant('{commonappdata}\WinSched'), True);
  if Result then
    Result := SaveStringToFile(
      ExpandConstant('{commonappdata}\WinSched\setup-provenance.txt'),
      'WinSched Setup controlled directory' + #13#10,
      False);
  if Result then
    Result :=
      RunSecurityScript(ExpandConstant('{commonappdata}\WinSched'), True);
end;

function HardenApplicationDirectory: Boolean;
begin
  Result := RunSecurityScript(ExpandConstant('{app}'), False);
end;

function ClearProvisionReceipt: Boolean;
var
  ReceiptPath: String;
begin
  ReceiptPath := ExpandConstant('{commonappdata}\WinSched\provision-result.txt');
  Result := (not FileExists(ReceiptPath)) or DeleteFile(ReceiptPath);
  Result := Result and (not FileExists(ReceiptPath));
end;

function PriorGuiInstallPathIsCompatible: Boolean;
var
  PriorPath: String;
begin
  Result := True;
  if RegQueryStringValue(HKLM64, 'Software\WinSched', 'InstallPath', PriorPath) then
    Result := CompareText(PriorPath, ExpandConstant('{autopf}\WinSched')) = 0;
end;

function ValidateSetupDataMarker: Boolean;
var
  ScriptPath: String;
  Parameters: String;
  ResultCode: Integer;
  MarkerLines: TArrayOfString;
begin
  Result := False;
  try
    ExtractTemporaryFile('secure-data.ps1');
  except
    exit;
  end;
  ScriptPath := ExpandConstant('{tmp}\secure-data.ps1');
  Parameters :=
    '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' +
    ScriptPath + '" -Directory "' +
    ExpandConstant('{commonappdata}\WinSched') +
    '" -Purpose Data -ValidateControlFiles';
  if (not ExecQuiet(
       ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
       Parameters,
       ResultCode)) or
     (ResultCode <> 0) then
    exit;
  if not LoadStringsFromFile(
       ExpandConstant('{commonappdata}\WinSched\setup-provenance.txt'),
       MarkerLines) then
    exit;
  if GetArrayLength(MarkerLines) <> 1 then
    exit;
  Result :=
    CompareText(Trim(MarkerLines[0]), 'WinSched Setup controlled directory') = 0;
end;

function DataDirectoryProvenanceIsTrusted: Boolean;
var
  PriorPath: String;
begin
  if not DirExists(ExpandConstant('{commonappdata}\WinSched')) then begin
    Result := True;
    exit;
  end;
  if ExistingService then begin
    Result := True;
    exit;
  end;
  Result :=
    RegQueryStringValue(HKLM64, 'Software\WinSched', 'InstallPath', PriorPath) and
    (CompareText(PriorPath, ExpandConstant('{autopf}\WinSched')) = 0);
  if not Result then
    Result := ValidateSetupDataMarker;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  NeedsRestart := False;
  if CompareText(ExpandConstant('{app}'), ExpandConstant('{autopf}\WinSched')) <> 0 then begin
    Result := CustomMessage('InstallPathInvalid');
    exit;
  end;
  if not PriorGuiInstallPathIsCompatible then begin
    Result := CustomMessage('LegacyInstallPathUnsupported');
    exit;
  end;
  StopTray;
  if not PriorServiceStateCaptured then begin
    ExistingService := ServiceExists;
    ServiceWasRunning := ExistingService and (not ServiceIsStopped);
    PriorServiceStateCaptured := True;
  end;
  if not DataDirectoryProvenanceIsTrusted then
    Result := CustomMessage('UntrustedDataDirectory')
  else if ExistingService and (not StopExistingService) then
    Result := CustomMessage('ServiceRemoveFailed')
  else if not HardenDataDirectory then
    Result := CustomMessage('DataDirectorySecurityFailed')
  else if not ClearProvisionReceipt then
    Result := CustomMessage('DataDirectorySecurityFailed')
  else if not HardenApplicationDirectory then
    Result := CustomMessage('InstallPathInvalid');
end;

function ProvisionReceiptIsSuccess: Boolean;
var
  ReceiptLines: TArrayOfString;
begin
  Result := False;
  if not LoadStringsFromFile(
       ExpandConstant('{commonappdata}\WinSched\provision-result.txt'),
       ReceiptLines) then
    exit;
  if GetArrayLength(ReceiptLines) <> 1 then
    exit;
  Result := CompareText(Trim(ReceiptLines[0]), 'SUCCESS') = 0;
end;

procedure VerifyProvisionService;
begin
  ProvisionAttempted := True;
  if not ProvisionReceiptIsSuccess then begin
    RaiseException(CustomMessage('ServiceInstallFailed'));
  end;
  ProvisionSucceeded := True;
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\provision-result.txt'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\winsched.exe'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\winsched-service.exe'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\winsched-tray.exe'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\winsched-monitor.exe'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\winsched-settings.exe'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\install.ps1'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\uninstall.ps1'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\secure-data.ps1'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\Install WinSched.cmd'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\Uninstall WinSched.cmd'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\README.md'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\LICENSE'));
  DeleteFile(ExpandConstant('{commonappdata}\WinSched\SHA256SUMS'));
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

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if ProvisionAttempted and (not ProvisionSucceeded) then
    Result := 9;
end;

procedure DeinitializeSetup;
var
  ResultCode: Integer;
begin
  if ExistingService and ServiceWasRunning and ServiceStoppedBySetup and
     (not ProvisionSucceeded) and ServiceIsStopped then begin
    if (not ExecQuiet(
      ExpandConstant('{sys}\sc.exe'),
      'start ' + ServiceName,
      ResultCode)) or (ResultCode <> 0) then
      Log('WinSched rollback restart through sc.exe failed');
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
     PurgeDataRequested then begin
    if (not DelTree(ExpandConstant('{commonappdata}\WinSched'), True, True, True)) or
       DirExists(ExpandConstant('{commonappdata}\WinSched')) then
      RaiseException(CustomMessage('PurgeDataFailed'));
    if RegValueExists(HKLM64, 'Software\WinSched', 'InstallPath') and
       (not RegDeleteValue(HKLM64, 'Software\WinSched', 'InstallPath')) then
      RaiseException(CustomMessage('PurgeDataFailed'));
    RegDeleteKeyIfEmpty(HKLM64, 'Software\WinSched');
  end;
end;
