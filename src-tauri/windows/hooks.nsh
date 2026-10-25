; Runs inside the Tauri NSIS installer, which runs as administrator.
;
; NetAgg is two processes from one netagg.exe: the NetAgg service (SYSTEM,
; started with --service) runs the engine, and the tray app runs as the user
; and asks the service for everything over a named pipe.

!macro NSIS_HOOK_PREINSTALL
  ; The service gives the PC its routing back as it stops; `net stop` waits
  ; for that, before netagg.exe is replaced. Disabled first, so its restart on
  ; failure can't start the old file again; one that doesn't stop (1.4.0
  ; builds before the fix could hang on exit) is ended. The post-install
  ; step sets it back to automatic.
  nsExec::Exec 'sc.exe config NetAgg start= disabled'
  nsExec::Exec 'net stop NetAgg'
  nsExec::Exec 'taskkill /F /FI "SERVICES eq NetAgg"'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; The service runs as SYSTEM, so only administrators may change its files:
  ; the permissions of C:\Program Files, wherever it was installed (a
  ; hand-made D:\Program Files usually lets every user change things). Set on
  ; the folder only, then the files below inherit them; the folder and files
  ; belong to Administrators.
  nsExec::Exec 'icacls "$INSTDIR" /inheritance:r /grant:r *S-1-5-32-544:(OI)(CI)F *S-1-5-18:(OI)(CI)F *S-1-5-32-545:(OI)(CI)RX'
  nsExec::Exec 'icacls "$INSTDIR\*" /reset /T /C /Q'
  nsExec::Exec 'icacls "$INSTDIR" /setowner *S-1-5-32-544 /T /C /Q'

  ; The service's settings (they choose its DNS servers), usage and log, in
  ; ProgramData\NetAgg. Anyone may create folders in ProgramData, so NetAgg
  ; itself checks it: a link there is removed (never followed), a folder
  ; someone else could have changed is moved aside, and it's created with its
  ; permissions (Administrators and SYSTEM change, Users read) from the start.
  ; The installing user's settings from before the service are taken over.
  nsExec::ExecToLog '"$INSTDIR\netagg.exe" --prepare-data-dir'
  Pop $0
  ${If} $0 <> 0
    DetailPrint "NetAgg couldn't make its settings folder safe; it won't turn on until it can."
  ${EndIf}

  ; Versions before the service started at sign-in through an elevated
  ; scheduled task; the tray app now uses the user's Run key.
  nsExec::Exec 'schtasks /Delete /TN NetAgg /F'

  ; Created on the first install; pointed at this install on every one.
  ; Restarted if it ever crashes.
  nsExec::Exec 'sc.exe create NetAgg binPath= "\"$INSTDIR\netagg.exe\" --service" start= auto DisplayName= NetAgg'
  nsExec::Exec 'sc.exe config NetAgg binPath= "\"$INSTDIR\netagg.exe\" --service" start= auto'
  nsExec::Exec 'sc.exe description NetAgg "Runs the NetAgg engine: spreads connections across your networks, with the DNS settings you chose."'
  nsExec::Exec 'sc.exe failure NetAgg reset= 86400 actions= restart/5000/restart/30000//'
  nsExec::Exec 'sc.exe start NetAgg'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; As before installing: stopped for sure before its file goes.
  nsExec::Exec 'sc.exe config NetAgg start= disabled'
  nsExec::Exec 'net stop NetAgg'
  nsExec::Exec 'taskkill /F /FI "SERVICES eq NetAgg"'
  ; An update (/UPDATE) keeps the service: the new version's installer
  ; points it at the new files.
  ${If} $UpdateMode <> 1
    nsExec::Exec 'sc.exe delete NetAgg'
    nsExec::Exec 'schtasks /Delete /TN NetAgg /F'
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; "Delete the application data" also takes the service's.
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ReadEnvStr $0 ProgramData
    RMDir /r "$0\NetAgg"
  ${EndIf}
!macroend
