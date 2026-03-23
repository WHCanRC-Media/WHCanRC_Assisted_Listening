!include "MUI2.nsh"

Name "WHCanRC Assisted Listening"
OutFile "whcanrc-assisted-listening-setup.exe"
InstallDir "$PROGRAMFILES\WHCanRC Assisted Listening"
RequestExecutionLevel admin

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "Install"
    SetOutPath $INSTDIR

    ; Copy the binary
    File "..\whcanrc-assisted-listening-windows-x86_64.exe"
    Rename "$INSTDIR\whcanrc-assisted-listening-windows-x86_64.exe" "$INSTDIR\whcanrc-assisted-listening.exe"

    ; Write default config if it doesn't exist
    IfFileExists "$INSTDIR\config.toml" +2
    File "/oname=config.toml" "..\config.toml.example"

    ; Install and start the Windows Service
    nsExec::ExecToLog '"$INSTDIR\whcanrc-assisted-listening.exe" --install-service'

    ; Add firewall rule
    nsExec::ExecToLog 'netsh advfirewall firewall add rule name="WHCanRC Assisted Listening" dir=in action=allow protocol=TCP localport=8080'
    nsExec::ExecToLog 'netsh advfirewall firewall add rule name="WHCanRC Assisted Listening UDP" dir=in action=allow protocol=UDP localport=8080'

    ; Start the service
    nsExec::ExecToLog 'sc start WHCanRCAssistedListening'

    ; Create uninstaller
    WriteUninstaller "$INSTDIR\uninstall.exe"

    ; Add to Add/Remove Programs
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\WHCanRCAssistedListening" "DisplayName" "WHCanRC Assisted Listening"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\WHCanRCAssistedListening" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\WHCanRCAssistedListening" "Publisher" "WHCanRC"
SectionEnd

Section "Uninstall"
    ; Stop and remove the service
    nsExec::ExecToLog 'sc stop WHCanRCAssistedListening'
    nsExec::ExecToLog 'sc delete WHCanRCAssistedListening'

    ; Remove firewall rules
    nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="WHCanRC Assisted Listening"'
    nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="WHCanRC Assisted Listening UDP"'

    ; Remove files
    Delete "$INSTDIR\whcanrc-assisted-listening.exe"
    Delete "$INSTDIR\uninstall.exe"
    ; Leave config.toml in case user wants to preserve settings
    RMDir "$INSTDIR"

    ; Remove registry entries
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\WHCanRCAssistedListening"
SectionEnd
