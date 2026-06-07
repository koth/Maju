## Why

Kodex currently requires users to manually download and install new desktop builds. Adding Tauri updater support lets released clients discover signed updates from GitHub Releases and install them without leaving the app.

## What Changes

- Add Tauri v2 updater support to the desktop shell with signed update artifacts.
- Configure the updater to read release metadata from GitHub Releases via `latest.json`.
- Add a frontend update check/install path that can be surfaced from settings or startup UI.
- Add a GitHub Actions release workflow that builds signed Windows and macOS installers and uploads updater metadata.
- Document the required release secrets and tag-driven release flow.

## Capabilities

### New Capabilities
- `desktop-auto-update`: Desktop clients can discover, verify, download, and install signed application updates distributed through GitHub Releases.

### Modified Capabilities

## Impact

- `apps/desktop/src-tauri/tauri.conf.json` and release config: updater endpoint, public key placeholder, updater artifact generation.
- `apps/desktop/src-tauri/Cargo.toml` and `src/main.rs`: updater/process plugin dependencies and initialization.
- `apps/desktop/src-tauri/capabilities/default.json`: updater/process permissions.
- `apps/desktop/ui`: updater API wiring and user-facing update controls.
- `.github/workflows/`: release workflow for Windows and macOS updater artifacts.
- `README.md` or release documentation: signing key and GitHub secret setup.
