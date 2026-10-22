; SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
; SPDX-License-Identifier: GPL-3.0-or-later
; See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
;
; Windows installer for Helia (NSIS 3). Built by packaging/windows/build-installer.ps1:
;   makensis /DVERSION=0.5.0 /DVERSION4=0.5.0.0 /DSTAGE=<dir> /DOUTFILE=<exe> helia.nsi
;
; Per user by default: installs under %LOCALAPPDATA%\Programs\Helia with no admin
; rights. The "Who is this for?" page offers all users (Program Files); choosing it
; as a standard user asks for elevation by relaunching the installer. Switches, all
; optional:
;   /S                 silent
;   /AllUsers          all users (needs administrator rights); the default is per user
;   /D=C:\Dir          install folder (must be the last switch, no quotes)
;   /NOSTART           no Start menu shortcut      /DESKTOP    add a desktop shortcut
;   /ADDPATH           add helia.exe to PATH       /NOASSOC    no "Open with Helia"
; The uninstaller is in Apps & Features and takes /S too.

Unicode true
SetCompressor /SOLID lzma
ManifestDPIAware true
RequestExecutionLevel user

!ifndef VERSION
  !error "pass /DVERSION=x.y.z"
!endif
!ifndef VERSION4
  !define VERSION4 "${VERSION}.0"
!endif
!ifndef STAGE
  !error "pass /DSTAGE=<staging dir>"
!endif
!ifndef OUTFILE
  !define OUTFILE "Helia-${VERSION}-setup.exe"
!endif

!define APP "Helia"
!define PUBLISHER "Laurence Horrocks-Barlow"
!define URL "https://github.com/qisback/helia"
!define ARP_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"
!define PROGID "Helia.Capture"

Name "${APP} ${VERSION}"
OutFile "${OUTFILE}"
; A sentinel: a real /D=folder replaces it before .onInit runs, so .onInit can tell the two apart.
InstallDir "*DEFAULT*"
BrandingText "${APP} ${VERSION} - ${PUBLISHER}"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "WinMessages.nsh"
!include "nsDialogs.nsh"

!insertmacro GetParameters
!insertmacro GetOptions
!insertmacro GetSize
!insertmacro un.GetParameters
!insertmacro un.GetOptions

VIProductVersion "${VERSION4}"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "Copyright (C) 2026 ${PUBLISHER}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!define MUI_ICON "${STAGE}\helia.ico"
!define MUI_UNICON "${STAGE}\helia.ico"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TEXT "This installs ${APP} ${VERSION}, a stacker for video of the Sun.$\r$\n$\r$\n${APP} is dedicated to the author's daughter, the light of his life.$\r$\n$\r$\nThe next two pages show the licence (GPL 3 or later) and the additional terms that go with it."
!define MUI_FINISHPAGE_RUN "$INSTDIR\helia-gui.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP}"

Var Args
Var Mode
Var CustomDir
Var RadioMe
Var RadioAll

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${STAGE}\LICENSE.txt"
!insertmacro MUI_PAGE_LICENSE "${STAGE}\ADDITIONAL-TERMS.txt"
Page custom ModePageCreate ModePageLeave
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; PowerShell edits the PATH value in place, keeping its type (REG_EXPAND_SZ) and
; unexpanded entries. $$ is a literal $ in NSIS. ROOT is CurrentUser or LocalMachine.
!macro AddPath ROOT SUBKEY
  nsExec::Exec 'powershell.exe -NoProfile -NonInteractive -Command "$$k=[Microsoft.Win32.Registry]::${ROOT}.OpenSubKey($\'${SUBKEY}$\',$$true);$$p=$$k.GetValue($\'Path$\',$\'$\',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames);$$d=$\'$INSTDIR$\';if(($\';$\'+$$p+$\';$\') -notlike ($\'*;$\'+$$d+$\';*$\')){$$k.SetValue($\'Path$\',(($$p.TrimEnd($\';$\')+$\';$\'+$$d).TrimStart($\';$\')),[Microsoft.Win32.RegistryValueKind]::ExpandString)}"'
  Pop $0
!macroend
!macro RemovePath ROOT SUBKEY
  nsExec::Exec 'powershell.exe -NoProfile -NonInteractive -Command "$$k=[Microsoft.Win32.Registry]::${ROOT}.OpenSubKey($\'${SUBKEY}$\',$$true);$$p=$$k.GetValue($\'Path$\',$\'$\',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames);$$d=$\'$INSTDIR$\';$$n=($$p -split $\';$\' | Where-Object { $$_ -and $$_ -ne $$d }) -join $\';$\';$$k.SetValue($\'Path$\',$$n,[Microsoft.Win32.RegistryValueKind]::ExpandString)"'
  Pop $0
!macroend

Section "Helia (required)" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "${STAGE}\helia-gui.exe"
  File "${STAGE}\helia.exe"
  File "${STAGE}\helia.ico"
  File "${STAGE}\LICENSE.txt"
  File "${STAGE}\ADDITIONAL-TERMS.txt"
  File "${STAGE}\LOGO-LICENCE.md"
  File "${STAGE}\README.md"
  File "${STAGE}\CHANGELOG.md"
  File "${STAGE}\THIRD-PARTY-LICENSES.txt"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr SHCTX "Software\${APP}" "InstallDir" "$INSTDIR"
  WriteRegStr SHCTX "Software\${APP}" "Version" "${VERSION}"
  WriteRegStr SHCTX "${ARP_KEY}" "DisplayName" "${APP}"
  WriteRegStr SHCTX "${ARP_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr SHCTX "${ARP_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr SHCTX "${ARP_KEY}" "DisplayIcon" "$INSTDIR\helia-gui.exe,0"
  WriteRegStr SHCTX "${ARP_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr SHCTX "${ARP_KEY}" "URLInfoAbout" "${URL}"
  WriteRegStr SHCTX "${ARP_KEY}" "HelpLink" "${URL}/issues"
  WriteRegStr SHCTX "${ARP_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe" /$Mode'
  WriteRegStr SHCTX "${ARP_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /$Mode /S'
  WriteRegDWORD SHCTX "${ARP_KEY}" "NoModify" 1
  WriteRegDWORD SHCTX "${ARP_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD SHCTX "${ARP_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "Start menu shortcut" SecStart
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\helia-gui.exe" "" "$INSTDIR\helia-gui.exe" 0
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\helia-gui.exe" "" "$INSTDIR\helia-gui.exe" 0
SectionEnd

Section /o "Add the helia command to PATH" SecPath
  ${If} $Mode == "AllUsers"
    !insertmacro AddPath "LocalMachine" "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"
  ${Else}
    !insertmacro AddPath "CurrentUser" "Environment"
  ${EndIf}
  WriteRegDWORD SHCTX "Software\${APP}" "PathAdded" 1
  SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
SectionEnd

Section "Open with Helia for SER, AVI and FITS files" SecAssoc
  ; Lists Helia under "Open with"; it never changes which program is the default.
  WriteRegStr SHCTX "Software\Classes\${PROGID}" "" "Helia capture"
  WriteRegStr SHCTX "Software\Classes\${PROGID}\DefaultIcon" "" "$INSTDIR\helia-gui.exe,0"
  WriteRegStr SHCTX "Software\Classes\${PROGID}\shell\open" "FriendlyAppName" "${APP}"
  WriteRegStr SHCTX "Software\Classes\${PROGID}\shell\open\command" "" '"$INSTDIR\helia-gui.exe" "%1"'
  WriteRegStr SHCTX "Software\Classes\Applications\helia-gui.exe" "FriendlyAppName" "${APP}"
  WriteRegStr SHCTX "Software\Classes\Applications\helia-gui.exe\shell\open\command" "" '"$INSTDIR\helia-gui.exe" "%1"'
  WriteRegStr SHCTX "Software\Classes\Applications\helia-gui.exe\SupportedTypes" ".ser" ""
  WriteRegStr SHCTX "Software\Classes\Applications\helia-gui.exe\SupportedTypes" ".avi" ""
  WriteRegStr SHCTX "Software\Classes\Applications\helia-gui.exe\SupportedTypes" ".fits" ""
  WriteRegStr SHCTX "Software\Classes\.ser\OpenWithProgids" "${PROGID}" ""
  WriteRegStr SHCTX "Software\Classes\.avi\OpenWithProgids" "${PROGID}" ""
  WriteRegStr SHCTX "Software\Classes\.fits\OpenWithProgids" "${PROGID}" ""
  WriteRegDWORD SHCTX "Software\${APP}" "FileTypes" 1
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} "The desktop app (helia-gui), the command line tool (helia), the licences and the notices."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecStart} "Adds Helia to the Start menu."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "Puts a Helia shortcut on the desktop."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecPath} "Lets you run helia from any terminal window. Open a new terminal afterwards."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAssoc} "Adds Helia to the Open with list for .ser, .avi and .fits files. It does not become the default program."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  Delete "$INSTDIR\helia-gui.exe"
  Delete "$INSTDIR\helia.exe"
  Delete "$INSTDIR\helia.ico"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\ADDITIONAL-TERMS.txt"
  Delete "$INSTDIR\LOGO-LICENCE.md"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\CHANGELOG.md"
  Delete "$INSTDIR\THIRD-PARTY-LICENSES.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  ReadRegDWORD $0 SHCTX "Software\${APP}" "PathAdded"
  ${If} $0 == 1
    ${If} $Mode == "AllUsers"
      !insertmacro RemovePath "LocalMachine" "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"
    ${Else}
      !insertmacro RemovePath "CurrentUser" "Environment"
    ${EndIf}
    SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}
  DeleteRegValue SHCTX "Software\Classes\.ser\OpenWithProgids" "${PROGID}"
  DeleteRegValue SHCTX "Software\Classes\.avi\OpenWithProgids" "${PROGID}"
  DeleteRegValue SHCTX "Software\Classes\.fits\OpenWithProgids" "${PROGID}"
  DeleteRegKey SHCTX "Software\Classes\${PROGID}"
  DeleteRegKey SHCTX "Software\Classes\Applications\helia-gui.exe"
  DeleteRegKey SHCTX "${ARP_KEY}"
  DeleteRegKey SHCTX "Software\${APP}"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

; ---- install mode and start-up ---------------------------------------------------
; Callbacks come after the sections so the section ids exist.

Function SetMode
  ; $Mode is CurrentUser or AllUsers: sets the shell context (and with it SHCTX) and, unless
  ; the user gave a folder with /D= or on the directory page, the default folder.
  ${If} $Mode == "AllUsers"
    SetShellVarContext all
    ${If} $CustomDir == ""
      StrCpy $INSTDIR "$PROGRAMFILES64\${APP}"
    ${EndIf}
  ${Else}
    SetShellVarContext current
    ${If} $CustomDir == ""
      StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${APP}"
    ${EndIf}
  ${EndIf}
FunctionEnd

Function IsAdmin
  ; $0 = 1 when running with administrator rights.
  UserInfo::GetAccountType
  Pop $0
  ${If} $0 == "Admin"
    StrCpy $0 1
  ${Else}
    StrCpy $0 0
  ${EndIf}
FunctionEnd

Function .onInit
  SetRegView 64
  StrCpy $Mode "CurrentUser"
  StrCpy $CustomDir ""
  ${If} $INSTDIR != "*DEFAULT*"
    StrCpy $CustomDir $INSTDIR
  ${EndIf}
  ${GetParameters} $Args
  ClearErrors
  ${GetOptions} $Args "/AllUsers" $0
  ${IfNot} ${Errors}
    StrCpy $Mode "AllUsers"
    Call IsAdmin
    ${If} $0 == 0
      MessageBox MB_OK|MB_ICONSTOP "Installing for all users needs administrator rights. Run the installer as an administrator, or leave out /AllUsers." /SD IDOK
      SetErrorLevel 740
      Abort
    ${EndIf}
  ${EndIf}
  Call SetMode
  ClearErrors
  ${GetOptions} $Args "/NOSTART" $0
  ${IfNot} ${Errors}
    SectionSetFlags ${SecStart} 0
  ${EndIf}
  ClearErrors
  ${GetOptions} $Args "/DESKTOP" $0
  ${IfNot} ${Errors}
    SectionSetFlags ${SecDesktop} ${SF_SELECTED}
  ${EndIf}
  ClearErrors
  ${GetOptions} $Args "/ADDPATH" $0
  ${IfNot} ${Errors}
    SectionSetFlags ${SecPath} ${SF_SELECTED}
  ${EndIf}
  ClearErrors
  ${GetOptions} $Args "/NOASSOC" $0
  ${IfNot} ${Errors}
    SectionSetFlags ${SecAssoc} 0
  ${EndIf}
FunctionEnd

Function ModePageCreate
  !insertmacro MUI_HEADER_TEXT "Who is this for?" "Choose who can use ${APP} on this computer."
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateRadioButton} 0 10u 100% 12u "Just for me (recommended, no administrator rights needed)"
  Pop $RadioMe
  ${NSD_CreateRadioButton} 0 30u 100% 12u "All users of this computer (needs administrator rights)"
  Pop $RadioAll
  ${If} $Mode == "AllUsers"
    ${NSD_Check} $RadioAll
  ${Else}
    ${NSD_Check} $RadioMe
  ${EndIf}
  nsDialogs::Show
FunctionEnd

Function ModePageLeave
  ${NSD_GetState} $RadioAll $1
  ${If} $1 == ${BST_CHECKED}
    ${If} $Mode != "AllUsers"
      Call IsAdmin
      ${If} $0 == 0
        ; Not elevated: start this installer again with administrator rights.
        System::Call 'shell32::ShellExecute(p 0, t "runas", t "$EXEPATH", t "/AllUsers", p 0, i 1) p .r0'
        ${If} $0 > 32
          Quit
        ${EndIf}
        MessageBox MB_OK|MB_ICONEXCLAMATION "Administrator rights were not granted. Choose Just for me, or try again."
        Abort
      ${EndIf}
      StrCpy $Mode "AllUsers"
      Call SetMode
    ${EndIf}
  ${Else}
    ${If} $Mode != "CurrentUser"
      StrCpy $Mode "CurrentUser"
      Call SetMode
    ${EndIf}
  ${EndIf}
FunctionEnd

Function un.onInit
  SetRegView 64
  ${un.GetParameters} $Args
  StrCpy $Mode "CurrentUser"
  ClearErrors
  ${un.GetOptions} $Args "/AllUsers" $0
  ${IfNot} ${Errors}
    StrCpy $Mode "AllUsers"
    UserInfo::GetAccountType
    Pop $0
    ${If} $0 != "Admin"
      ${IfNot} ${Silent}
        ; Relaunch elevated with the same switches.
        System::Call 'shell32::ShellExecute(p 0, t "runas", t "$EXEPATH", t "$Args", p 0, i 1) p .r0'
        ${If} $0 > 32
          Quit
        ${EndIf}
      ${EndIf}
      SetErrorLevel 740
      Abort
    ${EndIf}
    SetShellVarContext all
  ${Else}
    SetShellVarContext current
  ${EndIf}
FunctionEnd
