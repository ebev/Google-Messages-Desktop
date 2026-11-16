; dual-arch installer launcher for Google Messages (x64 + ARM64)
;
; This is NOT the real installer. It embeds both Tauri-generated NSIS
; installers, detects the host's native CPU architecture, and silently
; runs the matching one. Tauri's installer keeps owning every real
; install concern: WebView2, registry, shortcuts, uninstaller.
; The launcher leaves nothing behind (payloads live in $PLUGINSDIR,
; auto-deleted on exit) and creates no uninstall entry of its own.
;
; Compile-time defines (passed by release.yml):
;   CI_PRODUCT     display name        (e.g. "Google Messages")
;   CI_VERSION     crate version       (e.g. 1.4.0)
;   CI_PAYLOAD_X64 x64 setup.exe path
;   CI_PAYLOAD_ARM64 arm64 setup.exe path
;   CI_OUTFILE     output exe path
;
; NOTE: the inner Tauri installers default to per-user installs
; (tauri.conf installMode) — no elevation needed here. If the app ever
; switches to perMachine installs, this launcher needs
; RequestExecutionLevel admin and a manifest review.

!include "LogicLib.nsh"

!ifndef CI_PRODUCT
  !define CI_PRODUCT "Google Messages"
!endif
!ifndef CI_VERSION
  !define CI_VERSION "0.0.0"
!endif
!ifndef CI_OUTFILE
  !define CI_OUTFILE "Google Messages_${CI_VERSION}_x64-arm64-setup.exe"
!endif

Name "${CI_PRODUCT} v${CI_VERSION} (x64 + ARM64)"
OutFile "${CI_OUTFILE}"
RequestExecutionLevel user
ShowInstDetails show
AutoCloseWindow false

Page instfiles

Section "install"
  SetOutPath "$PLUGINSDIR"
  File "/oname=setup-x64.exe" "${CI_PAYLOAD_X64}"
  File "/oname=setup-arm64.exe" "${CI_PAYLOAD_ARM64}"

  ; Native-arch detection: the CentralProcessor Identifier starts with
  ; "ARM64" on Windows-on-ARM hosts and "AMD64"/"x86" family strings on
  ; the rest. First five characters are enough.
  ReadRegStr $R0 HKLM "HARDWARE\DESCRIPTION\System\CentralProcessor\0" "Identifier"
  StrCpy $R1 "$R0" 5
  DetailPrint "Host CPU: $R0"

  ${If} $R1 == "ARM64"
    DetailPrint "ARM64 Windows detected — launching ARM64 installer (silent)"
    ExecWait '"$PLUGINSDIR\setup-arm64.exe" /S' $R2
  ${Else}
    DetailPrint "x64 Windows detected — launching x64 installer (silent)"
    ExecWait '"$PLUGINSDIR\setup-x64.exe" /S' $R2
  ${EndIf}

  ${If} $R2 == 0
    DetailPrint "Install complete."
  ${Else}
    DetailPrint "Install failed — exit code $R2"
    SetErrorLevel $R2
  ${EndIf}
SectionEnd
