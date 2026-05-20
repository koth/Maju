!macro NSIS_HOOK_POSTINSTALL
  IfFileExists "$INSTDIR\codex-acp\codex-acp.exe" 0 done
  CreateDirectory "$PROFILE\.kodex"
  CreateDirectory "$PROFILE\.kodex\bin"
  CopyFiles /SILENT "$INSTDIR\codex-acp\codex-acp.exe" "$PROFILE\.kodex\bin\codex-acp.exe"
done:
!macroend
