!macro NSIS_HOOK_POSTINSTALL
  CreateDirectory "$PROFILE\.kodex"
  CreateDirectory "$PROFILE\.kodex\bin"
  IfFileExists "$INSTDIR\codex-acp\codex-acp.exe" 0 claude
  CopyFiles /SILENT "$INSTDIR\codex-acp\codex-acp.exe" "$PROFILE\.kodex\bin\codex-acp.exe"
claude:
  IfFileExists "$INSTDIR\bundled-claude-agent-acp\claude-agent-acp.exe" 0 done
  CopyFiles /SILENT "$INSTDIR\bundled-claude-agent-acp\claude-agent-acp.exe" "$PROFILE\.kodex\bin\claude-agent-acp.exe"
done:
!macroend
