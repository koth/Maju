## Context

Kodex already has app settings for agent/provider configuration and a workspace launcher for local folders. The `remote-linux-workspace-acp-tcp` change adds the lower-level remote Linux workspace lifecycle: SSH launch, ACP over TCP, remote workspace identity, and local-only operation guards.

The current user-facing remote entry is still shaped like a one-off connection form. That is acceptable for early validation, but it is not a durable workflow for repeat use. Users should be able to configure remote machines once, validate them from Settings, and then open remote directories from the same Workbench patterns used for local workspaces.

## Goals / Non-Goals

**Goals:**

- Add a Remote section to Settings for saved remote Linux machines.
- Persist remote machine profiles in app-level configuration without storing Agent selection, private key contents, passphrases, or raw passwords.
- Validate a remote machine before opening a workspace and show diagnostics for SSH, the default remote home or an explicit remote path, optional Agent command, and ACP readiness phases.
- Let users open a remote directory from Workbench using a saved remote machine and an Agent chosen at open time.
- Keep local workspace opening visually primary on welcome and Workbench surfaces.
- Reuse the existing remote Linux ACP TCP launcher for actual remote workspace opens.

**Non-Goals:**

- Reimplementing SSH launch or ACP TCP transport; this change consumes the existing remote launcher capability.
- Full remote file tree, remote editor file IO, remote git, remote terminal, or remote LSP support beyond what the remote workspace implementation already provides.
- Managing SSH private keys, passphrases, or passwords in a new credential vault. One-time password entry for validation/open is allowed, but it is not profile storage.
- Supporting non-Linux remote hosts.
- Providing VS Code Remote-SSH parity or extension hosting.

## Decisions

### 1. Store remote machine profiles separately from remote workspace records

Introduce a saved profile model such as:

```rust
pub struct RemoteMachineProfile {
    pub id: Uuid,
    pub display_name: String,
    pub ssh_target: String,
    pub ssh_port: Option<u16>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_validation: Option<RemoteMachineValidation>,
}
```

The profile describes how to reach a machine only. It does not describe what Agent to run or persist credentials. A remote workspace record still includes the selected remote root and Agent. Opening a remote directory creates a `RemoteOpenRequest` from `profile_id + remote_root + agent_cli`, plus an optional one-time SSH password when the user supplies one, then delegates to the existing remote workspace open path. Remote workspace records are peers of local workspace records in open-workspace lists and recents; opening a remote directory MUST NOT remove or hide currently open local workspaces.

Alternative considered: store every remote root as a full machine+path record only in recent workspaces. Rejected because it makes validation and reuse awkward; the same machine would be duplicated for every project directory.

### 2. Persist profiles in app-level settings/config storage

Remote machines are global app configuration, similar to agent settings, not per-session state. Store them under `AppPaths::config_dir()` in the same app-settings persistence layer or an adjacent JSON file. Session storage and recent workspace storage can reference profile ids and copy display metadata for history.

Alternative considered: store profiles in SQLite session storage. Rejected because profiles need to be editable before any workspace/session is active and should not be tied to session lifecycle cleanup.

### 3. Validation is a staged backend probe

Add a backend validation API that returns structured phase results:

- SSH reachability/authentication.
- Remote path existence and directory check. If Settings validation has no path, validate the remote user's home directory (`~`) by default.
- Agent command availability or launch dry-run where an Agent is supplied by the Workbench open flow.
- ACP TCP readiness check when a full workspace probe is requested.

Each phase returns status, elapsed time, and a sanitized diagnostic message. The UI should render these as a compact checklist and preserve the last successful/failed result on the profile.

Alternative considered: validation only when opening a workspace. Rejected because Settings should let users confirm configuration before committing to a workspace switch.

### 4. Keep credential handling outside profile persistence

Profiles may store an SSH target, port, username embedded in the target, and SSH config alias. They MUST NOT store Agent defaults, private key contents, passphrases, raw passwords, or auth hint text. Validation and open flows MAY accept a one-time SSH password that is passed only to the SSH process and cleared from UI state after the action. Authentication otherwise uses the user's system SSH configuration, ssh-agent, or future OS-backed credential integration. If future credential storage is needed, profiles should store only an opaque reference to an OS-backed secret.

Alternative considered: add password/key fields to the remote setup form. Rejected because it expands security scope and creates a poor first impression for users who already rely on SSH config and agents.

### 5. Workbench remote open is guided, not a login dialog

Workbench should provide an "Open Remote Directory" affordance near existing workspace actions. If no profile exists, it opens Settings Remote or an inline guided empty state. If profiles exist, the user selects a machine, enters or picks a recent remote path, chooses the Agent for this open, can run validation, then opens the workspace.

The welcome screen remains local-first: the primary action opens a local folder, while remote is a secondary path that reuses saved profiles and links to Settings when setup is missing.

Alternative considered: keep showing all remote connection fields directly on the welcome surface. Rejected because it makes the product feel like a raw SSH login tool and distracts from the common local path.

### 6. Remote state is explicit in labels and unavailable actions

Remote workspaces should show host/path labels in recents, headers, and workspace switch controls. If remote path validation fails, the app remains on the current workspace or welcome state. Local-only features stay disabled or return explicit unsupported-remote errors until a remote implementation exists.

Alternative considered: allow local commands to attempt paths from the remote root string. Rejected because it risks treating `/home/user/project` as a desktop filesystem path.

## Risks / Trade-offs

- **[Risk] Profiles contain secrets by accident** -> Mitigation: define DTO fields without password/private-key material, add persistence tests, and sanitize validation diagnostics.
- **[Risk] Validation becomes slow or blocks the UI** -> Mitigation: run validation asynchronously with bounded timeouts and phase-level progress/results.
- **[Risk] Saved profile is stale when SSH config changes** -> Mitigation: keep validation repeatable and show last checked time/status rather than assuming stored data is always valid.
- **[Risk] Opening a remote directory disrupts the current local workspace on failure** -> Mitigation: perform validation/open preparation before replacing the active `Application`; switch only after remote connection succeeds.
- **[Risk] The same machine is saved multiple times** -> Mitigation: warn on duplicate normalized `ssh_target + port` while still allowing explicit duplicates when the display names differ.
- **[Risk] Feature scope grows into remote file browsing** -> Mitigation: path entry and recent paths are in scope; full remote browsing is deferred unless an existing remote filesystem implementation already supports it.

## Migration Plan

1. Add remote profile DTOs and persistence defaults; absence of the remote profile file means an empty profile list.
2. Add backend commands for listing, saving, deleting, and validating remote profiles.
3. Add Settings Remote UI and tests without changing workspace open behavior.
4. Add Workbench remote-open entry points that call the existing remote workspace open command.
5. Update recents/session labels to reference saved profile display metadata when available.
6. Keep existing local workspace and welcome behavior as regression tests.

Rollback is straightforward before profiles are widely used: hide the Remote Settings and Workbench entry points while leaving persisted profile files ignored. Existing local workspaces continue to load because remote profiles are additive app configuration.

## Open Questions

- Should validation include a full ACP launch by default, or reserve that heavier probe for an explicit "Test agent" action?
- Should remote paths be typed only in the first version, or should we add a remote directory picker once a remote filesystem service exists?
- Should profiles support per-profile default remote directories, or rely only on recent remote paths?
