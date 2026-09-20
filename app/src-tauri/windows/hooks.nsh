; freeseries registers a logon task for "Start with Windows" (src/autostart.rs). It has to go when
; the app goes, or Windows keeps trying to start an exe that no longer exists.
;
; Not during an update: the installer runs this uninstaller first, and deleting the task there
; would silently switch "Start with Windows" off with every new version.
;
; The freeseries audio device stays installed. Removing a driver needs administrator rights, which
; this per-user uninstaller doesn't have; Settings > Apps lists it as "FxSound Audio Enhancer".
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    nsExec::Exec 'schtasks.exe /Delete /TN "freeseries" /F'
    Pop $0
  ${EndIf}
!macroend
