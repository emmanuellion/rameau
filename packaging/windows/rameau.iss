; Windows installer (Inno Setup 6). Built by the Release workflow:
;   iscc /DAppVersion=0.2.0 /DExe=target\release\rameau.exe /Odist packaging\windows\rameau.iss
; Installs for the current user (no admin rights), adds a Start menu shortcut and an uninstaller.
; rameau.ico: the app icon at 16 to 256 px (PNG entries), made from assets/icon.png.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Exe
  #define Exe "..\..\target\release\rameau.exe"
#endif

[Setup]
; Never change AppId: Windows uses it to find the installed version on upgrade and uninstall.
AppId={{6F3A2C1E-8B4D-4E7A-9C2B-5D1E7F0A3B64}
AppName=Rameau
AppVersion={#AppVersion}
AppVerName=Rameau {#AppVersion}
AppPublisher=Emmanuel Lion
AppPublisherURL=https://emmanuellion.github.io/rameau/
AppSupportURL=https://github.com/emmanuellion/rameau/issues
AppUpdatesURL=https://github.com/emmanuellion/rameau/releases
DefaultDirName={autopf}\Rameau
DefaultGroupName=Rameau
DisableProgramGroupPage=yes
DisableDirPage=auto
DisableReadyPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
SetupIconFile=rameau.ico
UninstallDisplayIcon={app}\rameau.ico
UninstallDisplayName=Rameau
OutputBaseFilename=rameau-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
ShowLanguageDialog=no
CloseApplications=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"

[CustomMessages]
en.NoGit=Rameau uses the Git installed on your computer, and Git was not found.%n%nInstall Git for Windows (git-scm.com) before using Rameau. Open the download page now?
fr.NoGit=Rameau utilise le Git installé sur ton ordinateur, et Git est introuvable.%n%nInstalle Git pour Windows (git-scm.com) avant d'utiliser Rameau. Ouvrir la page de téléchargement maintenant ?

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Exe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "rameau.ico"; DestDir: "{app}"
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"

[Icons]
Name: "{autoprograms}\Rameau"; Filename: "{app}\rameau.exe"; IconFilename: "{app}\rameau.ico"
Name: "{autodesktop}\Rameau"; Filename: "{app}\rameau.exe"; IconFilename: "{app}\rameau.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\rameau.exe"; Description: "{cm:LaunchProgram,Rameau}"; Flags: nowait postinstall skipifsilent

[Code]
// Rameau runs the user's git: say so before installing if it is missing, without blocking.
function InitializeSetup(): Boolean;
var
  Code: Integer;
begin
  Result := True;
  if WizardSilent() then
    Exit;
  if Exec(ExpandConstant('{cmd}'), '/C where git >NUL 2>NUL', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0) then
    Exit;
  if MsgBox(CustomMessage('NoGit'), mbConfirmation, MB_YESNO) = IDYES then
    ShellExec('open', 'https://git-scm.com/download/win', '', '', SW_SHOWNORMAL, ewNoWait, Code);
end;
