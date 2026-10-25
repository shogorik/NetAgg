; Runs inside the Tauri NSIS installer.

!macro NSIS_HOOK_POSTINSTALL
  ; NetAgg runs elevated, and Start with Windows starts it that way without
  ; asking, so only administrators may change its files: the permissions of
  ; C:\Program Files, wherever it was installed (a hand-made D:\Program Files
  ; usually lets every user change things). Set on the folder only, then the
  ; files below inherit them; the folder and files belong to Administrators.
  nsExec::Exec 'icacls "$INSTDIR" /inheritance:r /grant:r *S-1-5-32-544:(OI)(CI)F *S-1-5-18:(OI)(CI)F *S-1-5-32-545:(OI)(CI)RX'
  nsExec::Exec 'icacls "$INSTDIR\*" /reset /T /C /Q'
  nsExec::Exec 'icacls "$INSTDIR" /setowner *S-1-5-32-544 /T /C /Q'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Start with Windows registers an elevated task; don't leave it behind.
  ; An update (/UPDATE) keeps it: the new version starts the same way.
  ${If} $UpdateMode <> 1
    nsExec::Exec 'schtasks /Delete /TN NetAgg /F'
  ${EndIf}
!macroend
