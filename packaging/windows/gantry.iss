; Inno Setup script for Gantry. Compiled by packaging/windows/build-installer.ps1 (or by .github/workflows/release.yml
; on a Windows runner). ISCC must be given: AppVersion, StageDir, OutputDir, OutputBaseFilename.
#ifndef AppVersion
  #error AppVersion is required (pass /DAppVersion=... to ISCC)
#endif
#ifndef StageDir
  #error StageDir is required (pass /DStageDir=... to ISCC)
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif
#ifndef OutputBaseFilename
  #define OutputBaseFilename "gantry-setup"
#endif

[Setup]
; A stable AppId so new versions upgrade in place. Do not change it.
AppId={{A3D8B1F4-6C2E-4A79-B5D3-9E7F0C1A2B84}
AppName=Gantry
AppVersion={#AppVersion}
AppPublisher=Gantry
AppPublisherURL=https://github.com/idogrf/gantry
AppSupportURL=https://github.com/idogrf/gantry/issues
AppUpdatesURL=https://github.com/idogrf/gantry/releases
DefaultDirName={autopf}\Gantry
DefaultGroupName=Gantry
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\gantry.exe
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseFilename}
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
SetupIconFile=gantry.ico
ChangesEnvironment=yes
LicenseFile={#StageDir}\LICENSE-MIT

[Tasks]
Name: "addtopath"; Description: "Add Gantry to PATH (run 'gantry' from any terminal)"; GroupDescription: "Options:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Options:"; Flags: unchecked

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{group}\Gantry"; Filename: "{app}\gantry.exe"
Name: "{group}\Uninstall Gantry"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Gantry"; Filename: "{app}\gantry.exe"; Tasks: desktopicon

[Registry]
; Append the install dir to the user PATH when the task is chosen; Inno broadcasts the environment change itself.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
  Tasks: addtopath; Check: NeedsAddPath('{app}')

[Run]
Filename: "{app}\gantry.exe"; Description: "Launch Gantry"; Flags: nowait postinstall skipifsilent

[Code]
function NeedsAddPath(Param: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Param := ExpandConstant(Param);
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;
