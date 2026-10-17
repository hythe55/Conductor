; Hooks run by the Tauri NSIS installer (elevated, since it installs per machine).

!macro NSIS_HOOK_PREINSTALL
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
    MessageBox MB_ICONEXCLAMATION "Conductor was installed, but its background service couldn't be set up, so other devices can't control this PC yet. Try installing again."
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog '"$INSTDIR\conductor-host.exe" uninstall'
  Pop $0
!macroend
