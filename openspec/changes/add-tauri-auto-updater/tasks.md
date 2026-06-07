## 1. Tauri Updater Configuration

- [x] 1.1 Add Rust and JavaScript updater/process plugin dependencies.
- [x] 1.2 Initialize updater/process plugins in the Tauri desktop shell.
- [x] 1.3 Configure updater artifact generation, GitHub Release endpoint, Windows install mode, and required capabilities.

## 2. Frontend Update Flow

- [x] 2.1 Add a typed frontend updater helper around Tauri updater/process APIs.
- [x] 2.2 Add user-facing update controls to the settings page with checking, available, installing, installed, and error states.
- [x] 2.3 Add focused frontend tests for update state transitions where practical.

## 3. GitHub Release Workflow

- [x] 3.1 Add a GitHub Actions release workflow for Windows x64, macOS Intel, and macOS Apple Silicon.
- [x] 3.2 Ensure the workflow installs dependencies, builds bundled agents, signs updater artifacts, uploads signatures, and generates `latest.json`.

## 4. Documentation And Verification

- [x] 4.1 Document updater signing key generation, GitHub Secrets, version/tag release flow, and macOS/Windows signing caveats.
- [x] 4.2 Run relevant frontend and Rust validation commands, updating lockfiles as needed.
