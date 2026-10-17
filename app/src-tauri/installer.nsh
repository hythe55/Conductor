; Hooks run by the Tauri NSIS installer (elevated, since it installs per machine).

!macro NSIS_HOOK_PREINSTALL
  ; Silent installs are updates run by the service as SYSTEM. Close the app here:
  ; the installer's own check can fail to close it from there, which stops the
  ; install. The service reopens the app afterwards.
  ${If} ${Silent}
    nsExec::Exec 'taskkill /F /IM conductor-app.exe'
    Pop $0
  ${EndIf}
  ; Stop the running service first so its files can be replaced during updates.
  ${If} ${FileExists} "$INSTDIR\conductor-host.exe"
    nsExec::ExecToLog '"$INSTDIR\conductor-host.exe" uninstall'
    Pop $0
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Registers the Conductor service and a firewall rule limited to Tailscale addresses.
  nsExec::ExecToLog '"$INSTDIR\conductor-host.exe" install'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONEXCLAMATION "Conductor was installed, but its background service couldn't be set up, so other devices can't control this PC yet. Try installing again." /SD IDOK
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog '"$INSTDIR\conductor-host.exe" uninstall'
  Pop $0
!macroend

; If an install stops partway, the service has already been removed. Put it
; back, so this PC can still be reached and updated.
Function .onInstFailed
  ${If} ${FileExists} "$INSTDIR\conductor-host.exe"
    nsExec::ExecToLog '"$INSTDIR\conductor-host.exe" install'
    Pop $0
  ${EndIf}
FunctionEnd
