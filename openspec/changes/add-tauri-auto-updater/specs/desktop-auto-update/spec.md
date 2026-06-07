## ADDED Requirements

### Requirement: Desktop client can check for updates
Kodex SHALL provide a user-facing desktop update check that uses the configured Tauri updater endpoint.

#### Scenario: No update is available
- **WHEN** the user checks for updates and the release endpoint reports no newer version
- **THEN** Kodex displays that the current version is up to date

#### Scenario: Update is available
- **WHEN** the user checks for updates and the release endpoint reports a newer signed version
- **THEN** Kodex displays the new version and allows the user to start installation

### Requirement: Desktop client installs only signed updates
Kodex MUST rely on Tauri updater signature verification before installing downloaded update artifacts.

#### Scenario: Update signature is invalid
- **WHEN** an update artifact does not match the configured updater public key
- **THEN** the updater rejects installation and Kodex displays the failure to the user

#### Scenario: Update installation completes
- **WHEN** a signed update downloads and installs successfully
- **THEN** Kodex offers or performs application relaunch so the new version can start

### Requirement: Release workflow publishes updater artifacts
The repository SHALL include a GitHub Actions release workflow that builds Windows and macOS desktop bundles and uploads updater metadata to GitHub Releases.

#### Scenario: Version tag is pushed
- **WHEN** a tag matching `app-v*` is pushed
- **THEN** GitHub Actions builds Windows x64, macOS Intel, and macOS Apple Silicon release assets

#### Scenario: Updater metadata is generated
- **WHEN** release assets are uploaded
- **THEN** the workflow uploads updater signatures and a `latest.json` manifest suitable for the configured GitHub Release updater endpoint

### Requirement: Release signing setup is documented
Kodex SHALL document the updater signing key setup and required GitHub repository secrets.

#### Scenario: Maintainer prepares releases
- **WHEN** a maintainer follows the release documentation
- **THEN** they can generate an updater key pair, configure the app public key, and add the private signing key to GitHub Secrets
