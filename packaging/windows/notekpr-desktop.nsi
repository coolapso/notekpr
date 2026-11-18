; The desktop app's Windows installer,
; notekpr-desktop_windows_amd64_setup.exe (common.nsh says how it
; is made): notekpr-desktop.exe in %LOCALAPPDATA%\Programs\notekpr, notekpr
; in the Start menu, and Microsoft Edge WebView2 Runtime (what draws the
; app's window) when the computer doesn't have it yet — Windows 11 does, as
; do most Windows 10 computers. Installing a newer one over it has the
; running app quit first and replaces it. Uninstalling removes the program,
; its Start menu entry and its starting at login; the app's settings and
; data (%APPDATA%\notekpr-desktop, %LOCALAPPDATA%\notekpr-desktop) and the
; Markdown folder stay.

!include "${__FILEDIR__}/common.nsh"

!define EXE "notekpr-desktop.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\notekpr-desktop"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define STARTUP_APPROVED_KEY "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"
; WebView2's Evergreen Bootstrapper, from Microsoft: it installs the latest
; runtime, which keeps itself up to date.
!define WEBVIEW2_ID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!define WEBVIEW2_BOOTSTRAPPER "https://go.microsoft.com/fwlink/p/?LinkId=2124703"

Name "notekpr"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\notekpr"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"

!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Open notekpr"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro NOTEKPR_VERSION_INFO "notekpr"

; The app, if it runs, quits before its program is replaced or removed. It
; is asked to as Quit in its tray asks (`--quit`: it stops its Markdown
; folder's sync first), then waited for until no copy runs the program any
; more — one for another profile may — which is when it can be opened for
; writing. In installer and uninstaller alike, hence a macro.
!macro QUIT_APP
  ${If} ${FileExists} "$INSTDIR\${EXE}"
    ExecWait '"$INSTDIR\${EXE}" --quit'
    StrCpy $R0 0
    ${Do}
      ClearErrors
      FileOpen $R1 "$INSTDIR\${EXE}" a
      ${IfNot} ${Errors}
        FileClose $R1
        ${Break}
      ${EndIf}
      IntOp $R0 $R0 + 1
      ; Half a second each: 30 seconds, then ask (or, run silently, end it).
      ${If} $R0 >= 60
        ${If} ${Silent}
          nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM ${EXE}'
          Pop $R1
        ${Else}
          MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "notekpr is still running. Quit it (Quit notekpr, in its menu in the tray), then choose Retry." IDRETRY +2
          Abort
        ${EndIf}
        StrCpy $R0 0
      ${EndIf}
      Sleep 500
    ${Loop}
  ${EndIf}
!macroend

Function .onInit
  !insertmacro NOTEKPR_CHECK_WINDOWS
FunctionEnd

Section "notekpr"
  ; The window needs WebView2 (Microsoft Edge's engine): installed for the
  ; whole computer or for this user, any version but 0.0.0.0. (This
  ; installer is a 32-bit program: HKLM\SOFTWARE is the 32-bit view,
  ; WOW6432Node, which is where Microsoft says to look.)
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_ID}" "pv"
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_ID}" "pv"
  ${EndIf}
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    DetailPrint "Installing Microsoft Edge WebView2 Runtime…"
    ; Downloaded, run only if Microsoft signed it, then removed.
    nsExec::ExecToLog `${POWERSHELL} "$$ErrorActionPreference = 'Stop'; [Net.ServicePointManager]::SecurityProtocol = 'Tls12'; $$f = Join-Path $$env:TEMP ('notekpr-webview2-' + [guid]::NewGuid() + '.exe'); Invoke-WebRequest -UseBasicParsing -Uri '${WEBVIEW2_BOOTSTRAPPER}' -OutFile $$f; $$s = Get-AuthenticodeSignature $$f; if ($$s.Status -ne 'Valid' -or $$s.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') { Remove-Item $$f; throw 'the download is not signed by Microsoft' }; $$p = Start-Process -FilePath $$f -ArgumentList '/silent', '/install' -Wait -PassThru; Remove-Item $$f; exit $$p.ExitCode"`
    Pop $0
    ${If} $0 != 0
      MessageBox MB_ICONEXCLAMATION "notekpr needs Microsoft Edge WebView2 Runtime, which couldn't be installed ($0). Get it from Microsoft (${WEBVIEW2_BOOTSTRAPPER}), then open notekpr." /SD IDOK
    ${EndIf}
  ${EndIf}

  SetOutPath "$INSTDIR"
  !insertmacro QUIT_APP
  File "/oname=${EXE}" "${BINARY}"
  File "/oname=LICENSE.txt" "LICENSE"
  File "/oname=THIRD_PARTY_LICENSES.txt" "docs/licenses.md"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\notekpr.lnk" "$INSTDIR\${EXE}"
  !insertmacro NOTEKPR_REGISTER "${UNINSTALL_KEY}" "notekpr" "$INSTDIR\${EXE}"
SectionEnd

Section "Uninstall"
  !insertmacro QUIT_APP

  ; Starting at login (the app's setting, one entry for each profile that
  ; turned it on): the entries that start this program, quoted or not.
  StrCpy $R3 "$INSTDIR\${EXE}"
  StrLen $R4 $R3
  StrCpy $R0 0
  ${Do}
    ClearErrors
    EnumRegValue $R1 HKCU "${RUN_KEY}" $R0
    ${If} ${Errors}
    ${OrIf} $R1 == ""
      ${Break}
    ${EndIf}
    ReadRegStr $R2 HKCU "${RUN_KEY}" $R1
    StrCpy $R5 $R2 1
    ${If} $R5 == '"'
      StrCpy $R2 $R2 "" 1
    ${EndIf}
    StrCpy $R2 $R2 $R4
    ${If} $R2 == $R3
      DeleteRegValue HKCU "${RUN_KEY}" $R1
      DeleteRegValue HKCU "${STARTUP_APPROVED_KEY}" $R1
    ${Else}
      IntOp $R0 $R0 + 1
    ${EndIf}
  ${Loop}

  Delete "$SMPROGRAMS\notekpr.lnk"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\THIRD_PARTY_LICENSES.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
