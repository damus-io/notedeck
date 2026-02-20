#ifndef BuildArch
  #define BuildArch ""
#endif

#if BuildArch != ""
  #define ExeSource "..\target\" + BuildArch + "-pc-windows-msvc\release\notedeck.exe"
  #define PkgOutputDir "..\packages\" + BuildArch
#else
  #define ExeSource "..\target\release\notedeck.exe"
  #define PkgOutputDir "..\packages"
#endif

[Setup]
AppName=Damus Notedeck
AppVersion=0.1
DefaultDirName={autopf}\Notedeck
DefaultGroupName=Damus Notedeck
OutputDir={#PkgOutputDir}
OutputBaseFilename=DamusNotedeckInstaller
Compression=lzma
SolidCompression=yes
#if BuildArch == "aarch64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif

[Files]
Source: "{#ExeSource}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Damus Notedeck"; Filename: "{app}\notedeck.exe"

[Run]
Filename: "{app}\notedeck.exe"; Description: "Launch Damus Notedeck"; Flags: nowait postinstall skipifsilent
