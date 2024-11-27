[Setup]
AppName=Damus Notedeck
AppVersion=0.1
DefaultDirName={pf}\Notedeck
DefaultGroupName=Damus Notedeck
OutputDir=packages
OutputBaseFilename=DamusNotedeckInstaller
Compression=lzma
SolidCompression=yes

[Files]
Source: "..\target\release\notedeck.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Damus Notedeck"; Filename: "{app}\notedeck.exe"

[Run]
Filename: "{app}\notedeck.exe"; Description: "Launch Damus Notedeck"; Flags: nowait postinstall skipifsilent
