; The CLI's Windows installer, notekpr-cli_windows_amd64_setup.exe
; (common.nsh says how it is made): notekpr.exe in
; %LOCALAPPDATA%\Programs\notekpr-cli, and that folder on the user's PATH,
; so `notekpr` runs in any new terminal. Installing a newer one over it
; replaces the program and keeps the rest; uninstalling takes the folder off
; PATH again. Profiles and local copies stay (%APPDATA%\notekpr,
; %LOCALAPPDATA%\notekpr): `notekpr logout --forget` removes one.

!include "${__FILEDIR__}/common.nsh"

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\notekpr-cli"
; The user's PATH, read and written through the registry as it is stored
; (%VARIABLES% unexpanded): NSIS's own strings end at 1024 characters, and a
; longer PATH would be cut short. The folder reaches PowerShell in
; NOTEKPR_DIR, so no path needs quoting.
!define PATH_KEY `$$k = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment'); $$p = [string]$$k.GetValue('Path', '', 'DoNotExpandEnvironmentNames')`

Name "notekpr CLI"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\notekpr-cli"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"

!define MUI_FINISHPAGE_TITLE "The notekpr CLI is installed"
!define MUI_FINISHPAGE_TEXT "Open a new terminal (PowerShell or Command Prompt) and sign in:$\r$\n$\r$\n    notekpr setup$\r$\n$\r$\nnotekpr --help lists every command."
!define MUI_FINISHPAGE_LINK "The CLI's documentation"
!define MUI_FINISHPAGE_LINK_LOCATION "${DOCS}/tools/cli/"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro NOTEKPR_VERSION_INFO "notekpr CLI"

Function .onInit
  !insertmacro NOTEKPR_CHECK_WINDOWS
FunctionEnd

Section "notekpr"
  SetOutPath "$INSTDIR"
  ; A notekpr that still runs (a sync) can't be replaced, but it can be
  ; renamed: it goes on with the old program until it stops, and the next
  ; one is the new. What an earlier update left behind goes now.
  Delete "$INSTDIR\notekpr.exe.old"
  ${If} ${FileExists} "$INSTDIR\notekpr.exe"
    Rename "$INSTDIR\notekpr.exe" "$INSTDIR\notekpr.exe.old"
  ${EndIf}
  File "/oname=notekpr.exe" "${BINARY}"
  File "/oname=LICENSE.txt" "LICENSE"
  File "/oname=THIRD_PARTY_LICENSES.txt" "docs/licenses.md"
  Delete "$INSTDIR\notekpr.exe.old"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; The folder goes onto PATH, unless it is there already.
  System::Call 'Kernel32::SetEnvironmentVariable(t "NOTEKPR_DIR", t "$INSTDIR")'
  nsExec::ExecToLog `${POWERSHELL} "${PATH_KEY}; if (($$p -split ';') -notcontains $$env:NOTEKPR_DIR) { $$k.SetValue('Path', ((@($$p.TrimEnd(';'), $$env:NOTEKPR_DIR) | Where-Object { $$_ }) -join ';'), 'ExpandString') }"`
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONEXCLAMATION "notekpr couldn't add $INSTDIR to your PATH ($0): add it yourself to run notekpr in any terminal." /SD IDOK
  ${EndIf}
  ; Programs started from now on (new terminals) see it.
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000

  ; uninstall.exe carries the installer's icon, notekpr's.
  !insertmacro NOTEKPR_REGISTER "${UNINSTALL_KEY}" "notekpr CLI" "$INSTDIR\uninstall.exe"
SectionEnd

Section "Uninstall"
  System::Call 'Kernel32::SetEnvironmentVariable(t "NOTEKPR_DIR", t "$INSTDIR")'
  nsExec::ExecToLog `${POWERSHELL} "${PATH_KEY}; $$n = (($$p -split ';') | Where-Object { $$_ -and $$_ -ne $$env:NOTEKPR_DIR }) -join ';'; if ($$n -ne $$p) { $$k.SetValue('Path', $$n, 'ExpandString') }"`
  Pop $0
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000

  Delete "$INSTDIR\notekpr.exe"
  Delete "$INSTDIR\notekpr.exe.old"
  ${If} ${FileExists} "$INSTDIR\notekpr.exe"
    MessageBox MB_ICONEXCLAMATION "notekpr is still running (a sync?), so $INSTDIR\notekpr.exe stays: stop it, then delete that folder." /SD IDOK
  ${EndIf}
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\THIRD_PARTY_LICENSES.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
