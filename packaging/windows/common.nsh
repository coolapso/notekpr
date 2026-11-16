; What the two Windows installers share (notekpr-cli.nsi, notekpr-desktop.nsi).
; Both install for the current user only, without administrator rights, under
; %LOCALAPPDATA%\Programs, with an uninstaller in "Installed apps"; both run
; silently with /S. .goreleaser.yaml makes them after each Windows build,
; from the checkout's top folder (paths here are relative to it):
;
;   makensis -NOCD -DVERSION=0.18.0 -DVERSION4=0.18.0.0 \
;     -DBINARY=path/to/the.exe -DOUTFILE=dist/clients/…_setup.exe \
;     packaging/windows/notekpr-desktop.nsi
;
; VERSION is the release's (or a snapshot's: 0.18.0-3-gabc1234), VERSION4
; its numbers as Windows' file properties want them.

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user
ManifestDPIAware true

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "WinVer.nsh"
!include "x64.nsh"
!include "FileFunc.nsh"
!include "WinMessages.nsh"

!define MUI_ICON "crates/desktop/icons/icon.ico"
!define MUI_UNICON "crates/desktop/icons/icon.ico"

!define HOMEPAGE "https://notekpr.md"
!define DOCS "https://docs.notekpr.md"

; Windows PowerShell, part of every Windows since 7, for what NSIS can't do
; on its own (PATH, downloading). Its commands go in -Command, which the
; execution policy doesn't apply to.
!define POWERSHELL `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command`

; The release's builds are x64: Windows 10 or newer on an x64 processor, or
; Windows 11 on ARM, which runs x64 programs (Windows 10 on ARM doesn't).
!macro NOTEKPR_CHECK_WINDOWS
  ${IfNot} ${AtLeastWin10}
    MessageBox MB_ICONSTOP "notekpr needs Windows 10 or newer." /SD IDOK
    Abort
  ${EndIf}
  ${If} ${IsNativeARM64}
    ${IfNot} ${AtLeastBuild} 22000
      MessageBox MB_ICONSTOP "notekpr needs Windows 11 on this computer (Windows 10 on ARM can't run it)." /SD IDOK
      Abort
    ${EndIf}
  ${ElseIfNot} ${IsNativeAMD64}
    MessageBox MB_ICONSTOP "notekpr needs 64-bit Windows." /SD IDOK
    Abort
  ${EndIf}
!macroend

; The installer's own file properties (VIProductVersion and the rest: after
; MUI_LANGUAGE, which defines the language they are in).
!macro NOTEKPR_VERSION_INFO PRODUCT
  VIProductVersion "${VERSION4}"
  VIFileVersion "${VERSION4}"
  VIAddVersionKey "ProductName" "${PRODUCT}"
  VIAddVersionKey "ProductVersion" "${VERSION}"
  VIAddVersionKey "FileVersion" "${VERSION}"
  VIAddVersionKey "FileDescription" "${PRODUCT} installer"
  VIAddVersionKey "CompanyName" "coolapso"
  VIAddVersionKey "LegalCopyright" "AGPL-3.0-or-later"
!macroend

; The entry in "Installed apps" (and its older "Programs and Features"),
; under HKCU\…\Uninstall\KEY, once everything is in $INSTDIR.
!macro NOTEKPR_REGISTER KEY NAME ICON
  WriteRegStr HKCU "${KEY}" "DisplayName" "${NAME}"
  WriteRegStr HKCU "${KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${KEY}" "Publisher" "coolapso"
  WriteRegStr HKCU "${KEY}" "URLInfoAbout" "${HOMEPAGE}"
  WriteRegStr HKCU "${KEY}" "HelpLink" "${DOCS}"
  WriteRegStr HKCU "${KEY}" "DisplayIcon" "${ICON}"
  WriteRegStr HKCU "${KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${KEY}" "EstimatedSize" "$0"
!macroend
