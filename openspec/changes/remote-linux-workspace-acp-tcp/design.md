## Context

Kodex currently opens a workspace from a local directory path and starts the configured ACP agent from the desktop host. The runtime already has two local transports: stdio and a TCP mode that spawns a local process with `--port <port>` and connects to `127.0.0.1:<port>`. The current TCP mode is still local: the workspace path is a local `PathBuf`, local filesystem commands assume direct disk access, and the child process lifecycle is managed by the desktop host.

Remote Linux workspaces need a different lifecycle. The user selects a remote host and path, Kodex uses SSH to start the agent on that Linux machine, the remote agent listens on a loopback TCP port, and Kodex reaches that port through an SSH tunnel or equivalent forwarded socket. After connection, ACP is the authority for agent interaction; local-only filesystem, git, terminal, and shell assumptions must be either disabled or explicitly routed through remote-capable services.

## Goals / Non-Goals

**Goals:**

- Open a remote Linux project directory from the local desktop UI.
- Launch the configured ACP agent on the Linux host through SSH.
- Connect local Kodex to the remote agent using ACP over TCP.
- Store remote workspace descriptors in recent/open workspace state without storing private key material or passphrases.
- Make remote sessions visually distinct and operationally safe when local-only commands are unavailable.
- Reuse existing ACP session, reducer, prompt, permission, and persistence flows where possible.

**Non-Goals:**

- General remote filesystem editing outside the ACP/session boundary.
- Windows or macOS remote hosts in the first iteration.
- Full VS Code Remote-SSH parity, remote extension hosting, or remote LSP hosting.
- Exposing unauthenticated TCP listeners on public interfaces.
- Synchronizing the entire remote repository to the local machine.

## Decisions

### Model remote workspaces explicitly

Introduce a workspace location model rather than overloading `WorkspaceDescriptor.root` as a plain local path. A local workspace remains a local path. A remote Linux workspace records host alias, user, port, remote root, display name, agent id, and connection metadata. The existing `root` string exposed to the UI can remain as a display/stable key during migration, but backend APIs must know whether the workspace is local or remote before touching the filesystem.

Alternative considered: encode remote paths as strings like `ssh://host/path` everywhere. This is simpler initially, but it makes path safety brittle because many backend modules currently convert workspace roots into `PathBuf` and call local filesystem APIs. An explicit location type makes unsafe local access easier to reject.

### Treat remote directories as peer workspaces

Remote Linux directories should appear next to local directories in the same workspace lists, recents, and session controls. A remote machine profile describes how to connect to a host; it is not itself a global workbench context. Opening a remote directory combines a saved machine profile with a remote absolute path and creates a `RemoteLinux` workspace entry whose identity is the host plus remote path.

This replaces the earlier host-first "remote window" direction. That approach made sense while the WOA channel could require a remote-specific configuration surface, but after removing WOA the Agent/provider settings are app-level concerns again. Keeping remote directories and local directories as peers makes workspace switching predictable and prevents a remote open from hiding local workspaces.

Alternative considered: keep remote-window semantics after connecting and hide all local workspaces. Rejected because remote machine settings no longer need a separate runtime/channel context, and hiding local entries makes a remote directory feel unlike the rest of the workspace model.

### Use SSH local forwarding for ACP TCP

The first implementation should start the agent on the remote host bound to `127.0.0.1:<remote_port>` and create a local forward from `127.0.0.1:<local_port>` to that remote loopback port. Kodex then uses the existing ACP TCP line transport against the local forwarded port.

Alternative considered: connect directly to a remote TCP listener. This avoids SSH port forwarding, but it creates a larger security surface and requires remote firewall configuration. SSH forwarding keeps authentication, encryption, and host reachability within the existing SSH trust boundary.

### Add a remote agent launcher instead of stretching the local process launcher

Keep the existing `HiddenAgentProcess` and local `TcpAgentProcess` behavior for local workspaces. Add a remote launcher that owns SSH process creation, readiness parsing, forwarded port allocation, stderr capture, and cleanup. The remote launcher should produce an ACP TCP endpoint for the runtime to connect to.

Alternative considered: make `TcpAgentProcess` accept arbitrary command wrappers such as `ssh host ...`. This hides remote lifecycle details inside shell quoting and makes cleanup/readiness/error handling hard to test.

### Treat ACP as the first remote boundary

Prompting, tool events, permissions, and session resume continue through ACP. Operations that currently require local disk access must either be remote-aware or unavailable for remote workspaces until a remote service exists. File tree, editor read/write, git status/diff, terminal, and LSP are the main audit points.

Alternative considered: mount the remote directory locally through SSHFS. This would preserve many local code paths, but adds platform-specific dependencies and failure modes outside Kodex's control. ACP-over-TCP should be the baseline path.

### Route initial remote file and Git operations through SSH command probes

Until ACP exposes a stable file/Git API, Kodex should provide a narrow remote workspace substrate by running bounded, JSON-speaking commands over the same SSH trust boundary used for agent bootstrap. File tree listing, editor read/write, Git status, Git stage, and Git diff review run against the configured remote project root and accept only sanitized relative paths from the UI. The implementation uses remote Node scripts so filesystem metadata, UTF-8 validation, content hashes, and Git porcelain parsing are structured rather than ad hoc shell text.

Alternative considered: keep file tree, editor, and Git disabled until the agent implements dedicated ACP tools. That is operationally safer, but it makes a connected remote workspace mostly unusable because users cannot inspect, edit, or review the remote project. The SSH command substrate is intentionally scoped and can be replaced by ACP-native APIs later without changing frontend DTOs.

### Keep credentials outside persistent workspace records

Remote workspace records may store host aliases, usernames, ports, and remote roots. They must not store private key contents or passphrases. The first implementation should rely on the user's SSH agent, standard SSH config, and OS credential prompts where available.

Alternative considered: build a credential vault into Kodex first. That is useful later, but it increases security scope and is not necessary for SSH-config-based workflows.

## Risks / Trade-offs

- Remote workspace APIs accidentally call local filesystem paths -> Gate all workspace operations on workspace location and add tests for remote rejection/routing.
- Agent starts but readiness cannot be detected reliably -> Require the remote launcher to own port selection and use active TCP connect retries with timeout and stderr diagnostics.
- Orphaned remote agent processes after app exit -> Keep the SSH session as the remote process owner and send shutdown on workspace close; document cleanup fallback.
- Local and remote workspace identifiers collide -> Use a normalized remote URI-style key for persistence, not only display name or remote path.
- SSH authentication failures are opaque -> Surface host, command phase, and stderr without exposing secrets.
- Remote git/file/editor features are incomplete at first -> Route supported operations through the remote SSH substrate and disable or label the remaining local-only UI affordances.

## Migration Plan

1. Add location-aware workspace DTOs and persistence defaults so existing local workspaces deserialize as local.
2. Add the remote launcher and ACP TCP endpoint path behind a new remote workspace command.
3. Audit local filesystem, git, terminal, LSP, and editor commands to reject remote workspaces with explicit errors until routed remotely.
4. Add UI entry points and remote labels after backend remote open works.
5. Add the initial SSH command-backed remote file/Git substrate for file tree, editor read/write, Git status/stage, and diff review.
6. Preserve existing local workspace behavior and keep local stdio/TCP tests passing.

Rollback is straightforward before remote workspaces are persisted: disable the remote open command and hide the UI entry point. After remote records exist, local workspace handling must ignore unknown remote entries rather than failing startup restore.

## Open Questions

- Should the remote agent choose its own port and print it, or should Kodex allocate and pass a remote port?
- Which SSH implementation should be used first: system `ssh` process or an embedded Rust SSH client?
- Should the SSH command-backed file/Git substrate be replaced by ACP-native APIs once agent support stabilizes?
- How should remote terminal support interact with the existing terminal dock?
