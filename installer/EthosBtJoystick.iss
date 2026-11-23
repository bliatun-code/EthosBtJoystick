; Installer for Ethos BT Joystick (Inno Setup 6).
;
; Built by tools\package-release.ps1, which passes the version and the folders:
;
;   ISCC /DAppVersion=1.2.0 /DStageDir=<folder with the exe and the license files>
;        /DOutputDir=<folder for the result> /DOutputName=<file name without .exe>
;        installer\EthosBtJoystick.iss
;
; This file is UTF-8 with a byte order mark; Inno Setup needs the mark to read
; the Norwegian texts correctly.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z - tools\package-release.ps1 does this
#endif
#ifndef StageDir
  #error Pass /DStageDir=<folder> - tools\package-release.ps1 does this
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif
#ifndef OutputName
  #define OutputName "EthosBtJoystick-v" + AppVersion + "-win-x64-setup"
#endif

#define AppName "Ethos BT Joystick"
#define AppExe "EthosBtJoystick.exe"
#define AppUrl "https://github.com/bliatun-code/EthosBtJoystick"
; The program keeps its driver files and install.log here (install_dir() in
; app\src\installer.rs), whatever folder the program itself is installed to.
#define DriverHome "{commonpf64}\EthosBtJoystick"

[Setup]
; Identifies the program to Windows. Never change it: an update would then be
; installed as a second program instead of replacing the first.
AppId={{6CB7D4CD-CA2A-4946-9990-4FD1126C4FA3}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=bliatun-code
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
VersionInfoVersion={#AppVersion}
VersionInfoDescription={#AppName} Setup

; One fixed folder, the same one the program's driver files live in.
DefaultDirName={autopf}\EthosBtJoystick
DisableDirPage=yes
DisableProgramGroupPage=yes

; The driver can only be installed with administrator rights.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 11
MinVersion=10.0.22000

; The program's single-instance mutex (app\src\main.rs): Setup and the
; uninstaller ask the user to close the program first.
AppMutex=EthosBtJoystick.SingleInstance
CloseApplications=no

SetupIconFile=..\app\assets\EthosBtJoystick.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
ShowLanguageDialog=auto
Compression=lzma2/max
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}

; The [Registry] entry below only removes the program's own "Start with
; Windows" value of the current user when the program is uninstalled.
UsedUserAreasWarning=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "no"; MessagesFile: "compiler:Languages\Norwegian.isl"

[Messages]
en.SetupAppRunningError=Setup has detected that %1 is running.%n%nPlease close it now (right-click its icon in the notification area and choose Exit), then click OK to continue, or Cancel to exit.
en.UninstallAppRunningError=Uninstall has detected that %1 is running.%n%nPlease close it now (right-click its icon in the notification area and choose Exit), then click OK to continue, or Cancel to exit.
no.SetupAppRunningError=Installasjonsprogrammet har oppdaget at %1 kjører.%n%nLukk programmet nå (høyreklikk ikonet i systemstatusfeltet og velg Avslutt), og klikk deretter OK for å fortsette, eller Avbryt for å avslutte.
no.UninstallAppRunningError=Avinstallasjonsprogrammet har oppdaget at %1 kjører.%n%nLukk programmet nå (høyreklikk ikonet i systemstatusfeltet og velg Avslutt), og klikk deretter OK for å fortsette, eller Avbryt for å avslutte.

[CustomMessages]
en.DriverGroup=Virtual joystick driver:
en.DriverTask=Install or update the driver (needed to use the radio as a joystick; adds a certificate for the driver to this PC)
en.DriverInstalling=Installing the virtual joystick driver...
en.DriverFailed=The virtual joystick driver could not be installed.%n%nThe program itself is installed. Start it and click "Install driver" to try again. The log is in:%n%1
no.DriverGroup=Driver for den virtuelle joysticken:
no.DriverTask=Installer eller oppdater driveren (nødvendig for å bruke radioen som joystick; legger inn et sertifikat for driveren på denne PC-en)
no.DriverInstalling=Installerer driveren for den virtuelle joysticken ...
no.DriverFailed=Driveren for den virtuelle joysticken kunne ikke installeres.%n%nSelve programmet er installert. Start det og klikk «Installer driver» for å prøve igjen. Loggen ligger i:%n%1

[Tasks]
Name: "driver"; Description: "{cm:DriverTask}"; GroupDescription: "{cm:DriverGroup}"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#StageDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\LICENSE-driver-MS-PL.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\THIRD-PARTY-LICENSES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; "Start with Windows" is switched on inside the program; make sure the entry
; does not outlive the program. Nothing is written when installing.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "EthosBtJoystick"; Flags: dontcreatekey uninsdeletevalue

[Run]
; Started as the user who launched Setup, not as administrator.
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Removes the device, the driver package and the certificate again.
Filename: "{app}\{#AppExe}"; Parameters: "--uninstall-driver"; RunOnceId: "RemoveDriver"; Flags: runhidden

[UninstallDelete]
Type: filesandordirs; Name: "{#DriverHome}\driver"
Type: files; Name: "{#DriverHome}\install.log"
Type: dirifempty; Name: "{#DriverHome}"

[Code]
// The program installs its own driver when it is started with
// --install-driver (silently; exit code 0 means success and the details are
// in install.log). Setup already runs as administrator, so no further prompt
// appears.
procedure InstallDriver;
var
  ResultCode: Integer;
begin
  WizardForm.StatusLabel.Caption := CustomMessage('DriverInstalling');
  WizardForm.FilenameLabel.Caption := '';
  WizardForm.ProgressGauge.Style := npbstMarquee;
  try
    if not Exec(ExpandConstant('{app}\{#AppExe}'), '--install-driver', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
      ResultCode := -1;
  finally
    WizardForm.ProgressGauge.Style := npbstNormal;
  end;
  if ResultCode <> 0 then
    SuppressibleMsgBox(FmtMessage(CustomMessage('DriverFailed'), [ExpandConstant('{#DriverHome}\install.log')]), mbError, MB_OK, IDOK);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('driver') then
    InstallDriver;
end;
