//! The browser provider installer, as the application layer sees it.
//!
//! `browser-service` owns the install mechanics; this module owns the
//! process-wide provisioner and the state the settings pane reads. Keeping it
//! here means the desktop shell depends on `app-core` only, which is the
//! dependency direction the rest of the codebase already follows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use browser_service::installer::{HostCommandRunner, PhaseSink, ProvisionDeps, Provisioner};
use browser_service::provision::CommandRunner;
use browser_service::provision::{HostFs, InstallPhase, ResolvedProvider};
use workspace_model::BrowserInstallState;

/// Resolve a build tool the way preflight resolves Node.
///
/// A packaged app is launched from Finder or the Dock and inherits a PATH of
/// `/usr/bin:/bin:/usr/sbin:/sbin` — no Homebrew, no version manager. Passing
/// the bare name `npm` to a spawn therefore fails to start even though the
/// user plainly has it, which is exactly what the install button reported:
/// preflight passed (it searches past PATH) and the install then failed with
/// `No such file or directory`. `dsh_bridge::find_binary` is the same search
/// preflight uses, so the two cannot disagree about whether the tool exists.
fn resolve_host_tool(name: &str) -> String {
    dsh_bridge::find_binary(name)
        .map(|path| path.to_string_lossy().into_owned())
        // Only when the tool genuinely is not installed, so the spawn error
        // names something the user can act on.
        .unwrap_or_else(|| name.to_string())
}

/// Put the resolved tools' directory on the install's `PATH`.
///
/// Resolving `npm` to an absolute path is only half the fix. `npm` is a script
/// that starts `node`, and it finds that by name on the `PATH` it inherits —
/// which, in the same GUI-launched process, is still the minimal one. Spawning
/// `/opt/homebrew/bin/npm` with `PATH=/usr/bin:/bin` therefore gets one step
/// further and fails as `env: node: No such file or directory`.
///
/// This is what a login shell's profile would have contributed, and nothing
/// else in the app is responsible for it.
fn install_env_with_tool_dirs(
    parent: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    const PATH_KEY: &str = "PATH";

    let mut env = parent.clone();
    let inherited = parent.get(PATH_KEY).cloned().unwrap_or_default();

    // Every tool this install runs, including `node` itself, so npm's own
    // startup finds what it needs. Prepended: a version manager earlier on the
    // list must win, and these are the same ones preflight resolved.
    let mut entries: Vec<String> = vec!["npm".to_string(), "npx".to_string(), "node".to_string()]
        .iter()
        .map(|name| PathBuf::from(resolve_host_tool(name)))
        .filter_map(|tool| tool.parent().map(|dir| dir.to_string_lossy().into_owned()))
        .filter(|dir| !dir.is_empty() && Path::new(dir).is_dir())
        .collect();
    entries.dedup();

    // Keep what was already there, minus any duplicate of what we just added.
    let existing = std::env::split_paths(&inherited)
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|dir| !dir.is_empty() && !entries.contains(dir))
        .collect::<Vec<_>>();

    entries.extend(existing);
    if !entries.is_empty() {
        env.insert(
            PATH_KEY.to_string(),
            std::env::join_paths(entries.iter().map(PathBuf::from))
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        );
    }
    env
}

/// The process-wide provisioner, created on first use.
///
/// One at a time is the point: two concurrent `npm` processes writing the same
/// tree produce a subtly broken `node_modules` that passes an exit-code check
/// and fails at runtime.
pub fn provisioner(paths: &crate::AppPaths) -> Arc<Provisioner> {
    static PROVISIONER: std::sync::OnceLock<Arc<Provisioner>> = std::sync::OnceLock::new();
    if let Some(existing) = PROVISIONER.get() {
        return existing.clone();
    }

    let settings = crate::settings::load_app_settings(paths);
    let npm = resolve_host_tool("npm");
    let npx = resolve_host_tool("npx");
    let parent_env = install_env_with_tool_dirs(&std::env::vars().collect());

    let deps = ProvisionDeps {
        resolved: ResolvedProvider::resolve(paths.root(), &settings.browser.provider_version),
        runner: Arc::new(HostCommandRunner) as Arc<dyn CommandRunner>,
        fs: Arc::new(HostFs),
        // A missing npm surfaces as a spawn error naming the tool, which is a
        // different remedy from a network failure and must not be conflated.
        npm,
        npx,
        // The browser install runs the pinned provider's own CLI, so it goes
        // through the same Node the provider will be launched with. Fetching a
        // different Playwright via `npx` is what put the wrong Chromium build on
        // the host in the first place.
        node: resolve_host_tool("node"),
        chromium_installed: crate::browser_preflight::playwright_browser_installed_on_host(),
        parent_env,
    };

    let created = Arc::new(Provisioner::new(deps));
    // Losing the race is harmless: the other thread installed a provisioner
    // with the same settings, and two of them would be worse than one.
    let _ = PROVISIONER.set(created.clone());
    created
}

/// A provisioner built from explicit parts, for tests.
pub fn provisioner_for_test(deps: ProvisionDeps) -> Arc<Provisioner> {
    Arc::new(Provisioner::new(deps))
}

/// Run an install, or join the one already running, and report the outcome.
pub fn install(paths: &crate::AppPaths) -> Result<BrowserInstallState, String> {
    install_with(&provisioner(paths))
}

/// Read the current install state without starting anything.
pub fn install_state(paths: &crate::AppPaths) -> BrowserInstallState {
    install_state_of(&provisioner(paths))
}

/// [`install`] against a caller-supplied provisioner.
pub fn install_with(provisioner: &Provisioner) -> Result<BrowserInstallState, String> {
    let phase = crate::shared_mcp::block_on(provisioner.provision())?;
    Ok(state_of(provisioner, phase))
}

/// [`install_state`] against a caller-supplied provisioner.
pub fn install_state_of(provisioner: &Provisioner) -> BrowserInstallState {
    let phase = provisioner
        .phase()
        // Nothing recorded yet: the pane shows "not started" rather than
        // claiming a phase that never happened.
        .unwrap_or(InstallPhase::Resolving);
    state_of(provisioner, phase)
}

/// Build the state the pane reads, from a phase and the live install check.
pub fn state_of(provisioner: &Provisioner, phase: InstallPhase) -> BrowserInstallState {
    let model_phase = match &phase {
        InstallPhase::Resolving => workspace_model::BrowserInstallPhase::Resolving,
        InstallPhase::InstallingPackage => workspace_model::BrowserInstallPhase::InstallingPackage,
        InstallPhase::InstallingChromium => {
            workspace_model::BrowserInstallPhase::InstallingChromium
        }
        InstallPhase::Verifying => workspace_model::BrowserInstallPhase::Verifying,
        InstallPhase::Complete { verified } => workspace_model::BrowserInstallPhase::Complete {
            verified: *verified,
        },
        InstallPhase::Failed { step, detail } => workspace_model::BrowserInstallPhase::Failed {
            step: step.clone(),
            detail: detail.clone(),
        },
    };

    BrowserInstallState {
        phase: model_phase,
        label: phase.label(),
        running: phase.is_running(),
        verified: phase.succeeded(),
        installed: provisioner.install_state().is_usable(),
    }
}

/// Subscribe to phase transitions, for the shell to forward onto its event
/// bus.
pub fn on_phase(paths: &crate::AppPaths, sink: Arc<dyn PhaseSink>) {
    provisioner(paths).on_phase(sink);
}

/// Re-exported so the shell does not need the lower crate.
pub use browser_service::installer::PhaseSink as InstallPhaseSink;
pub use browser_service::provision::InstallPhase as ProvisionPhase;

/// The install is spawned with a path, never with a bare tool name.
///
/// This is the defect the install button hit on a packaged app: `npm` resolves
/// through Homebrew, but a Finder- or Dock-launched process inherits a PATH of
/// `/usr/bin:/bin:/usr/sbin:/sbin`, so the spawn failed with `No such file or
/// directory` while preflight — using the same search — reported Node as
/// present. Every test in this file injects its own runner, so none of them
/// ever touched the resolution the production path depends on.
#[cfg(test)]
mod tool_resolution_tests {
    use super::resolve_host_tool;

    #[test]
    fn npm_resolves_to_an_existing_absolute_path_not_a_bare_name() {
        let Some(found) = dsh_bridge::find_binary("npm") else {
            // Nothing to assert on a host without npm; the fallback is still
            // the bare name, which is what makes this worth checking elsewhere.
            return;
        };

        let resolved = resolve_host_tool("npm");
        assert_eq!(resolved, found.to_string_lossy());
        assert!(
            std::path::Path::new(&resolved).is_file(),
            "resolved npm does not exist: {resolved}"
        );
        assert!(
            std::path::Path::new(&resolved).components().count() > 1,
            "a bare name cannot spawn from a GUI app's PATH: {resolved}"
        );
    }

    #[test]
    fn npx_resolves_too_because_the_chromium_step_needs_it() {
        if let Some(found) = dsh_bridge::find_binary("npx") {
            assert_eq!(resolve_host_tool("npx"), found.to_string_lossy());
        }
    }

    /// The two tools must agree, or a package install and a browser download
    /// would come from different Node installations.
    #[test]
    fn npm_and_npx_come_from_the_same_place() {
        if let (Some(npm), Some(npx)) = (
            dsh_bridge::find_binary("npm"),
            dsh_bridge::find_binary("npx"),
        ) {
            assert_eq!(
                npm.parent(),
                npx.parent(),
                "npm and npx resolved to different installations: {npm:?} {npx:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_service::installer::ProvisionDeps;
    use browser_service::provision::CommandOutcome;
    use std::collections::HashSet;
    use std::path::Path;
    use std::sync::Mutex;

    /// The flag both the runner and the filesystem read, so verification sees
    /// what the install actually did rather than a value set afterwards.
    #[derive(Default)]
    struct Installed {
        present: Mutex<bool>,
    }

    impl Installed {
        fn set(&self, value: bool) {
            *self.present.lock().unwrap() = value;
        }

        fn get(&self) -> bool {
            *self.present.lock().unwrap()
        }
    }

    struct OkRunner {
        installed: Arc<Installed>,
    }

    struct Fs {
        installed: Arc<Installed>,
    }

    impl browser_service::provision::ProvisionFs for Fs {
        fn exists(&self, _path: &Path) -> bool {
            self.installed.get()
        }
        fn is_dir(&self, _path: &Path) -> bool {
            self.installed.get()
        }
    }

    #[async_trait::async_trait]
    impl CommandRunner for OkRunner {
        async fn run(
            &self,
            _program: &str,
            _args: &[String],
            _env: &std::collections::HashMap<String, String>,
            _timeout: std::time::Duration,
        ) -> CommandOutcome {
            self.installed.set(true);
            CommandOutcome {
                success: true,
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                spawn_error: None,
                timed_out: false,
            }
        }
    }

    fn deps(runner: Arc<OkRunner>, fs: Arc<Fs>) -> ProvisionDeps {
        ProvisionDeps {
            resolved: ResolvedProvider::resolve(Path::new("/data"), "1.0.0"),
            runner,
            fs: fs as Arc<dyn browser_service::provision::ProvisionFs>,
            npm: "npm".to_string(),
            npx: "npx".to_string(),
            node: "node".to_string(),
            chromium_installed: true,
            parent_env: HashSet::new().into_iter().collect(),
        }
    }

    fn parts() -> (Arc<OkRunner>, Arc<Fs>) {
        let installed = Arc::new(Installed::default());
        (
            Arc::new(OkRunner {
                installed: installed.clone(),
            }),
            Arc::new(Fs { installed }),
        )
    }

    #[tokio::test]
    async fn state_reflects_a_verified_install() {
        let (runner, fs) = parts();
        let provisioner = provisioner_for_test(deps(runner, fs));

        let state = state_of(&provisioner, provisioner.provision().await);

        assert!(state.verified);
        assert!(!state.running);
        // The install check is live, not the phase's opinion of it.
        assert!(state.installed);
    }

    #[tokio::test]
    async fn an_unverified_install_is_not_reported_as_installed() {
        let (runner, fs) = parts();
        let provisioner = provisioner_for_test(deps(runner, fs));

        let state = state_of(&provisioner, InstallPhase::Complete { verified: false });

        assert!(!state.verified);
        assert!(
            !state.installed,
            "a phase claiming completion is not evidence the files are there",
        );
    }

    #[test]
    fn install_with_reports_a_verified_result() {
        // The command surface is a thin shell over this; these cover what the
        // shell actually forwards.
        let (runner, fs) = parts();
        let provisioner = provisioner_for_test(deps(runner, fs));

        let state = install_with(&provisioner).expect("install returns a state");

        assert!(state.verified);
        assert!(state.installed);
        assert!(!state.running);
    }

    #[test]
    fn install_state_before_anything_started_does_not_claim_a_phase_ran() {
        let (runner, fs) = parts();
        let provisioner = provisioner_for_test(deps(runner, fs));

        let state = install_state_of(&provisioner);

        assert!(!state.installed);
        assert!(!state.verified);
        // Resolving is the "not started yet" placeholder, and it still reads
        // as running so the pane can enable its controls.
        assert_eq!(state.phase, workspace_model::BrowserInstallPhase::Resolving);
    }

    #[test]
    fn install_state_after_a_finished_operation_reports_the_terminal_phase() {
        let (runner, fs) = parts();
        let provisioner = provisioner_for_test(deps(runner, fs));
        let _ = install_with(&provisioner);

        let state = install_state_of(&provisioner);
        assert!(state.verified);
        assert!(!state.running, "a finished operation is not still running");
    }

    // The join behaviour itself is covered in `installer` with a blocking
    // runner that can genuinely overlap two calls. Repeating it here would
    // assert nothing the shell adds, so it is not repeated.

    #[tokio::test]
    async fn a_failure_state_names_its_step_and_detail() {
        let (runner, fs) = parts();
        let provisioner = provisioner_for_test(deps(runner, fs));

        let state = state_of(
            &provisioner,
            InstallPhase::Failed {
                step: "Installing Chromium".to_string(),
                detail: "download failed".to_string(),
            },
        );

        assert!(!state.running);
        assert!(!state.verified);
        assert!(state.label.contains("失败"));
    }

    /// The real install, through the real resolution, into the real data root.
    ///
    /// Run it with a PATH that a packaged app would actually have:
    ///
    /// ```text
    /// env PATH=/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.cargo/bin \
    ///     cargo test -p app-core --lib -- --ignored real_install_from_app_paths
    /// ```
    ///
    /// That PATH is the whole defect: Homebrew and every version manager are
    /// absent from it, so a spawn by bare name fails while the tool is
    /// installed. The unit tests above all inject a fake runner, so none of
    /// them can reach this; before the fix this reported
    /// `无法启动安装程序：No such file or directory`.
    #[test]
    #[ignore = "performs a real npm install against the user's data root"]
    fn real_install_from_app_paths() {
        let paths = crate::AppPaths::resolve().expect("real data root");
        let state = install(&paths).unwrap_or_else(|error| panic!("install failed: {error}"));
        eprintln!("install state: {state:?}");
        assert!(state.verified, "not verified: {state:?}");
        assert!(state.installed, "not usable: {state:?}");
    }
}
