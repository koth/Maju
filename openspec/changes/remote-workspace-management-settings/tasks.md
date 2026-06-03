## 1. Remote Profile Model And Persistence

- [x] 1.1 Add workspace-model DTOs for remote machine profiles, profile input, validation phase results, validation snapshots, and remote open requests.
- [x] 1.2 Add app-core persistence for remote machine profiles under app config storage with empty-list defaults when no file exists.
- [x] 1.3 Enforce profile validation rules for required display name, SSH target, valid optional port, and no Agent or credential material persisted on machine profiles.
- [x] 1.4 Add backend tests for profile create/update/delete persistence, invalid profile rejection, empty defaults, and Agent/secret-field absence.

## 2. Remote Validation Backend

- [x] 2.1 Add app-core validation service that runs staged SSH, remote path, open-time Agent command, and optional ACP readiness probes with bounded timeouts.
- [x] 2.2 Sanitize validation diagnostics before returning them to the UI or persisting last validation results.
- [x] 2.3 Persist the latest validation result and checked timestamp on the validated profile.
- [x] 2.4 Add tests for successful validation, unreachable SSH target, missing remote path, unusable agent command, timeout handling, and diagnostic sanitization.

## 3. Desktop Commands And API Bindings

- [x] 3.1 Add Tauri commands for listing, saving, deleting, and validating remote machine profiles.
- [x] 3.2 Add a Tauri command for opening a remote directory from `profile_id + remote_root + agent_cli` using the existing remote Linux workspace launcher.
- [x] 3.3 Extend frontend Tauri bindings and TypeScript types for remote profile management, validation, and remote open requests.
- [x] 3.4 Ensure remote open failure does not replace the current active workspace or welcome state.

## 4. Settings Remote UI

- [x] 4.1 Add a Remote section/tab to the existing Settings UI with an empty state and add-profile action.
- [x] 4.2 Implement remote profile create, edit, delete, and duplicate-target warning flows.
- [x] 4.3 Render validation status, phase diagnostics, checked time, and retry actions for each profile.
- [x] 4.4 Add Settings UI tests for empty state, create/edit/delete, validation success, validation failure, and recoverable load/save errors.

## 5. Workbench Remote Open Flow

- [x] 5.1 Add an "Open Remote Directory" Workbench entry point near existing workspace open actions.
- [x] 5.2 Implement the guided remote-open flow for selecting a saved profile, entering a remote absolute path, choosing an Agent, validating it, and opening it.
- [x] 5.3 Route the welcome screen remote action through saved profiles or Remote settings while keeping local folder open as the primary action.
- [x] 5.4 Display remote host/profile and remote path labels in headers, workspace controls, and recent workspace entries.
- [x] 5.5 Keep local-only Workbench actions disabled or explicitly unsupported for active remote workspaces unless a remote implementation exists.
- [x] 5.6 Keep remote directory workspaces and local directory workspaces as peers instead of entering a global remote-machine context.
- [x] 5.7 Add the sidebar new-workspace menu with local folder and remote directory peer choices.

## 6. Verification

- [x] 6.1 Run backend tests covering remote profile persistence, validation, Tauri command behavior, and remote-open failure safety.
- [x] 6.2 Run frontend tests covering Settings Remote and Workbench remote-open flows.
- [ ] 6.3 Run a manual local workspace regression to confirm local opening remains primary and unchanged.
- [ ] 6.4 Run a manual remote smoke test with a reachable Linux host, saved profile validation, and remote directory open.
