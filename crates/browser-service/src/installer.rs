//! One provider install, from request to verified outcome.
//!
//! The install runs `npm` and then `playwright install`, which is a
//! user-visible operation lasting tens of seconds. This module owns its shape:
//! which phase it is in, that a second request joins rather than racing, and
//! — most importantly — that **verification decides success, not the exit
//! code**. `npm` can exit zero with a tree that does not contain the package
//! that was asked for, and reporting that as a working install is exactly the
//! failure this whole subsystem exists to remove.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::provision::{
    BROWSER_INSTALL_TIMEOUT, CommandOutcome, CommandRunner, INSTALL_TIMEOUT, InstallPhase,
    InstallState, ProvisionFs, ResolvedProvider, install_env, verify,
};

// `CommandRunner` is defined in `provision` and named in this module's public
// API (`ProvisionDeps::runner`), so the import is part of that surface rather
// than an implementation detail.
pub use crate::provision::CommandRunner as CommandRunnerApi;

/// Steps, named so a failure can say which one broke.
///
/// Chinese, because they are shown to the user in a failure label and the rest
/// of this enum's labels are.
const STEP_PACKAGE: &str = "安装浏览器 provider";
const STEP_CHROMIUM: &str = "安装 Chromium";
const STEP_VERIFY: &str = "校验安装结果";

/// The browser the provider launches by default.
///
/// `chrome-for-testing`, not `chromium`. The provider's `--browser` accepts
/// chrome, firefox, webkit and msedge — `chromium` is not one of them, so asking
/// for it by that name falls through to this default rather than erroring, which
/// is how a launch could look configured and still want a build nobody had.
pub const PROVIDER_BROWSER: &str = "chrome-for-testing";

/// argv for the provider's own browser installer: `<cli.js> install-browser …`.
///
/// Run through the resolved Node rather than through `npx`, so the install
/// cannot pick up a different Playwright — and therefore a different browser
/// revision — than the provider pinned.
pub fn browser_install_argv(entry_point: &std::path::Path, args: &[String]) -> Vec<String> {
    let mut argv = vec![entry_point.to_string_lossy().into_owned()];
    argv.push("install-browser".to_string());
    argv.extend(args.iter().cloned());
    argv
}

/// What the provisioner needs from the host.
pub struct ProvisionDeps {
    pub resolved: ResolvedProvider,
    pub runner: Arc<dyn CommandRunner>,
    pub fs: Arc<dyn ProvisionFs>,
    /// npm executable name, resolved from `PATH`.
    pub npm: String,
    /// npx executable name.
    pub npx: String,
    /// Node that runs the provider's own CLI, for the browser install step.
    ///
    /// The browser must come from the pinned package's Playwright, not from
    /// whatever `npx` would fetch, so the install is driven by this interpreter
    /// and the provider's own entry point.
    pub node: String,
    /// Whether a Chromium install is already present, so a package-only
    /// repair does not re-download browsers.
    pub chromium_installed: bool,
    /// The parent environment the install runs in.
    pub parent_env: std::collections::HashMap<String, String>,
}

impl ProvisionDeps {
    /// Whether the Chromium step has anything to do.
    fn needs_chromium(&self) -> bool {
        !self.chromium_installed
    }
}

/// Receives phase transitions.
pub trait PhaseSink: Send + Sync {
    fn on_phase(&self, phase: &InstallPhase);
}

/// A sink that records phases, for tests and for a caller that wants history.
#[derive(Default)]
pub struct RecordingSink(Mutex<Vec<InstallPhase>>);

impl RecordingSink {
    pub fn phases(&self) -> Vec<InstallPhase> {
        self.0
            .lock()
            .map(|phases| phases.clone())
            .unwrap_or_default()
    }
}

impl PhaseSink for RecordingSink {
    fn on_phase(&self, phase: &InstallPhase) {
        if let Ok(mut phases) = self.0.lock() {
            phases.push(phase.clone());
        }
    }
}

/// A running or finished install.
struct Operation {
    phase: InstallPhase,
}

/// Runs provider installs, one at a time.
pub struct Provisioner {
    deps: ProvisionDeps,
    current: Mutex<Option<Operation>>,
    /// Phase sinks, notified on every transition.
    listeners: Mutex<Vec<std::sync::Arc<dyn PhaseSink>>>,
}

impl Provisioner {
    pub fn new(deps: ProvisionDeps) -> Self {
        Self {
            deps,
            current: Mutex::new(None),
            listeners: Mutex::new(Vec::new()),
        }
    }

    pub fn resolved(&self) -> &ResolvedProvider {
        &self.deps.resolved
    }

    /// Whether a usable provider is present, from the same filesystem view
    /// verification uses.
    ///
    /// Answered from the provisioner's own `fs` rather than the host, so the
    /// reported state and the verification that produced it can never disagree.
    pub fn install_state(&self) -> InstallState {
        verify(&self.deps.resolved, self.deps.fs.as_ref())
    }

    /// The current phase, for a settings pane that was opened mid-install.
    pub fn phase(&self) -> Option<InstallPhase> {
        self.current
            .lock()
            .ok()
            .and_then(|current| current.as_ref().map(|op| op.phase.clone()))
    }

    pub fn on_phase(&self, listener: std::sync::Arc<dyn PhaseSink>) {
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.push(listener);
        }
    }

    fn publish(&self, phase: InstallPhase) {
        if let Ok(mut current) = self.current.lock()
            && let Some(operation) = current.as_mut()
        {
            operation.phase = phase.clone();
        }
        if let Ok(listeners) = self.listeners.lock() {
            for listener in listeners.iter() {
                listener.on_phase(&phase);
            }
        }
    }

    /// Start an install, or join the one already running.
    ///
    /// Two concurrent `npm` processes writing the same tree produce a subtly
    /// broken `node_modules` that passes an exit-code check and fails at
    /// runtime, so a second request joins rather than races.
    pub async fn provision(&self) -> InstallPhase {
        // Take the slot, or report what the holder is doing.
        {
            let mut current = match self.current.lock() {
                Ok(current) => current,
                Err(_) => {
                    return InstallPhase::Failed {
                        step: STEP_VERIFY.to_string(),
                        detail: "install state is poisoned".to_string(),
                    };
                }
            };
            if let Some(operation) = current.as_ref()
                && operation.phase.is_running()
            {
                return operation.phase.clone();
            }
            *current = Some(Operation {
                phase: InstallPhase::Resolving,
            });
        }

        self.publish(InstallPhase::Resolving);

        // Package.
        self.publish(InstallPhase::InstallingPackage);
        let env = install_env(&self.deps.parent_env);
        let package = self
            .deps
            .runner
            .run(
                &self.deps.npm,
                &self.deps.resolved.npm_install_args(),
                &env,
                INSTALL_TIMEOUT,
            )
            .await;
        if let Some(failure) = step_failure(&package) {
            return self.fail(STEP_PACKAGE, failure);
        }

        // Chromium, only when it is actually missing.
        if self.deps.needs_chromium() {
            self.publish(InstallPhase::InstallingChromium);
            // The provider's own installer, for the browser it will actually
            // use.
            //
            // `npx playwright install chromium` — what this used to run —
            // downloads whatever revision that `npx` happens to resolve, which
            // is the *bundled* chromium. The provider defaults to
            // `chrome-for-testing` and looks for its own revision, so a
            // successful install left the host with a Chromium the provider
            // refuses to start, and the first tool call failed with the
            // provider's own "not installed" error. Asking the pinned package
            // for its browser is the only request that cannot drift from the
            // revision it will launch.
            let args = browser_install_argv(
                &self.deps.resolved.entry_point,
                &[PROVIDER_BROWSER.to_string()],
            );
            let chromium = self
                .deps
                .runner
                .run(&self.deps.node, &args, &env, BROWSER_INSTALL_TIMEOUT)
                .await;
            if let Some(failure) = step_failure(&chromium) {
                return self.fail(STEP_CHROMIUM, failure);
            }
        }

        // Verification decides, not the exit code.
        self.publish(InstallPhase::Verifying);
        let state = self.install_state();
        let phase = match state {
            InstallState::Complete => InstallPhase::Complete { verified: true },
            // npm reported success but the provider is not there. Quoting the
            // entry point is what makes this actionable.
            InstallState::Broken => InstallPhase::Failed {
                step: STEP_VERIFY.to_string(),
                detail: format!(
                    "the installer finished but {} is missing",
                    self.deps.resolved.entry_point.display()
                ),
            },
            InstallState::Absent => InstallPhase::Failed {
                step: STEP_VERIFY.to_string(),
                detail: format!(
                    "nothing was installed at {}",
                    self.deps.resolved.package_root.display()
                ),
            },
        };
        self.publish(phase.clone());
        phase
    }

    fn fail(&self, step: &str, detail: String) -> InstallPhase {
        let phase = InstallPhase::Failed {
            step: step.to_string(),
            detail,
        };
        self.publish(phase.clone());
        phase
    }
}

/// The reason a step failed, or `None` when it succeeded.
fn step_failure(outcome: &CommandOutcome) -> Option<String> {
    if outcome.success {
        return None;
    }
    Some(outcome.failure_detail())
}

/// A runner backed by the real host, used in production.
pub struct HostCommandRunner;

#[async_trait]
impl CommandRunner for HostCommandRunner {
    async fn run(
        &self,
        program: &str,
        args: &[String],
        env: &std::collections::HashMap<String, String>,
        timeout: std::time::Duration,
    ) -> CommandOutcome {
        use std::process::Stdio;

        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // No console window: `npm`/`npx`/`node` are console-subsystem programs
        // run from a GUI app, and their output is captured below rather than
        // watched. See `crate::win`.
        crate::win::hide_console(&mut command);

        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return CommandOutcome {
                    success: false,
                    status: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    // The program is named because "No such file or directory"
                    // on its own does not say which of npm and npx was missing,
                    // and that is the whole question when a step fails to
                    // start. It most often means the tool is on the user's
                    // PATH but not the one this process inherited — a
                    // GUI-launched app gets a minimal one.
                    spawn_error: Some(format!("{program}: {error}")),
                    timed_out: false,
                };
            }
        };

        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => CommandOutcome {
                success: output.status.success(),
                status: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                spawn_error: None,
                timed_out: false,
            },
            Ok(Err(error)) => CommandOutcome {
                success: false,
                status: None,
                stdout: String::new(),
                stderr: String::new(),
                spawn_error: Some(error.to_string()),
                timed_out: false,
            },
            Err(_) => CommandOutcome {
                success: false,
                status: None,
                stdout: String::new(),
                stderr: String::new(),
                spawn_error: None,
                timed_out: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ScriptedRunner {
        outcomes: Mutex<Vec<CommandOutcome>>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
        /// Files the filesystem "has" once the package step has run.
        installed: AtomicUsize,
        /// Model npm exiting zero without producing a usable tree.
        produces_nothing: bool,
    }

    impl ScriptedRunner {
        fn new(outcomes: Vec<CommandOutcome>, installed: bool) -> Arc<Self> {
            Self::with_output(outcomes, installed, false)
        }

        /// `produces_nothing` models npm exiting zero without leaving a usable
        /// tree behind.
        fn with_output(
            outcomes: Vec<CommandOutcome>,
            installed: bool,
            produces_nothing: bool,
        ) -> Arc<Self> {
            Arc::new(Self {
                outcomes: Mutex::new(outcomes),
                calls: Mutex::new(Vec::new()),
                installed: AtomicUsize::new(if installed { 1 } else { 0 }),
                produces_nothing,
            })
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl CommandRunner for ScriptedRunner {
        async fn run(
            &self,
            program: &str,
            args: &[String],
            _env: &std::collections::HashMap<String, String>,
            _timeout: std::time::Duration,
        ) -> CommandOutcome {
            self.calls
                .lock()
                .unwrap()
                .push((program.to_string(), args.to_vec()));
            let mut outcomes = self.outcomes.lock().unwrap();
            if outcomes.is_empty() {
                return ok_outcome();
            }
            let outcome = outcomes.remove(0);
            // A failed step does not leave a usable package behind, which is
            // what makes the verification-override test meaningful.
            if outcome.success && !self.produces_nothing {
                self.installed.store(1, Ordering::SeqCst);
            }
            outcome
        }
    }

    fn ok_outcome() -> CommandOutcome {
        CommandOutcome {
            success: true,
            status: Some(0),
            stdout: "added 1 package".to_string(),
            stderr: String::new(),
            spawn_error: None,
            timed_out: false,
        }
    }

    fn fail_outcome(detail: &str) -> CommandOutcome {
        CommandOutcome {
            success: false,
            status: Some(1),
            stdout: String::new(),
            stderr: format!("npm error {detail}"),
            spawn_error: None,
            timed_out: false,
        }
    }

    /// Filesystem that reports the entry point present only after the runner
    /// has "installed" something, so verification reflects the real sequence.
    struct ScriptedFs {
        runner: Arc<ScriptedRunner>,
    }

    impl ProvisionFs for ScriptedFs {
        fn exists(&self, _path: &std::path::Path) -> bool {
            self.runner.installed.load(Ordering::SeqCst) == 1
        }

        fn is_dir(&self, _path: &std::path::Path) -> bool {
            self.runner.installed.load(Ordering::SeqCst) == 1
        }
    }

    fn provisioner(runner: Arc<ScriptedRunner>, chromium_installed: bool) -> Provisioner {
        Provisioner::new(ProvisionDeps {
            resolved: ResolvedProvider::resolve(std::path::Path::new("/data"), "1.2.3"),
            fs: Arc::new(ScriptedFs {
                runner: runner.clone(),
            }),
            runner,
            npm: "npm".to_string(),
            npx: "npx".to_string(),
            node: "node".to_string(),
            chromium_installed,
            parent_env: [("PATH".to_string(), "/usr/bin".to_string())]
                .into_iter()
                .collect(),
        })
    }

    #[tokio::test]
    async fn a_successful_install_reports_verified() {
        let runner = ScriptedRunner::new(vec![ok_outcome(), ok_outcome()], true);
        let phase = provisioner(runner, false).provision().await;
        assert_eq!(phase, InstallPhase::Complete { verified: true });
        assert!(phase.succeeded());
    }

    #[tokio::test]
    async fn both_steps_run_when_chromium_is_missing() {
        let runner = ScriptedRunner::new(vec![ok_outcome(), ok_outcome()], true);
        provisioner(runner.clone(), false).provision().await;
        assert_eq!(runner.call_count(), 2, "package then chromium");
    }

    #[tokio::test]
    async fn an_existing_chromium_skips_the_second_step() {
        // A package-only repair must not re-download browsers.
        let runner = ScriptedRunner::new(vec![ok_outcome()], true);
        let phase = provisioner(runner.clone(), true).provision().await;
        assert!(phase.succeeded());
        assert_eq!(runner.call_count(), 1, "only the package step");
    }

    #[tokio::test]
    async fn a_package_failure_stops_before_chromium() {
        let runner = ScriptedRunner::new(vec![fail_outcome("404 Not Found"), ok_outcome()], true);
        let phase = provisioner(runner.clone(), false).provision().await;

        match &phase {
            InstallPhase::Failed { step, detail } => {
                assert_eq!(step, STEP_PACKAGE);
                assert!(detail.contains("404"), "got {detail}");
            }
            other => panic!("expected a package failure, got {other:?}"),
        }
        assert_eq!(
            runner.call_count(),
            1,
            "chromium must not run after a failed package"
        );
    }

    #[tokio::test]
    async fn a_chromium_failure_fails_the_whole_operation() {
        let runner = ScriptedRunner::new(vec![ok_outcome(), fail_outcome("download failed")], true);
        let phase = provisioner(runner, false).provision().await;

        match &phase {
            InstallPhase::Failed { step, .. } => assert_eq!(step, STEP_CHROMIUM),
            other => panic!("expected a chromium failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verification_overrides_a_successful_exit_code() {
        // npm exiting zero with nothing installed is the failure this whole
        // subsystem exists to prevent.
        let runner = ScriptedRunner::with_output(vec![ok_outcome(), ok_outcome()], false, true);
        let phase = provisioner(runner, false).provision().await;

        match &phase {
            InstallPhase::Failed { step, detail } => {
                assert_eq!(step, STEP_VERIFY);
                assert!(detail.contains("nothing was installed"), "got {detail}");
            }
            other => panic!("verification should have failed it, got {other:?}"),
        }
    }

    /// A runner whose first call blocks, so a second request genuinely
    /// arrives while the first is still in flight.
    struct BlockingRunner {
        inner: Arc<ScriptedRunner>,
        gate: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl CommandRunner for BlockingRunner {
        async fn run(
            &self,
            program: &str,
            args: &[String],
            env: &std::collections::HashMap<String, String>,
            timeout: std::time::Duration,
        ) -> CommandOutcome {
            let first = self.inner.call_count() == 0;
            let outcome = self.inner.run(program, args, env, timeout).await;
            if first {
                // Hold the install open until the second request has arrived.
                self.gate.notified().await;
            }
            outcome
        }
    }

    #[tokio::test]
    async fn a_second_request_joins_the_running_install() {
        // Genuine concurrency: the second request has to arrive while the
        // first is still inside the runner, which is the only way the join
        // path is reachable.
        let inner = ScriptedRunner::new(vec![ok_outcome(), ok_outcome()], true);
        let gate = Arc::new(tokio::sync::Notify::new());
        let runner = Arc::new(BlockingRunner {
            inner: inner.clone(),
            gate: gate.clone(),
        });

        let provisioner = Arc::new(Provisioner::new(ProvisionDeps {
            resolved: ResolvedProvider::resolve(std::path::Path::new("/data"), "1.2.3"),
            fs: Arc::new(ScriptedFs {
                runner: inner.clone(),
            }),
            runner,
            npm: "npm".to_string(),
            npx: "npx".to_string(),
            node: "node".to_string(),
            chromium_installed: false,
            parent_env: Default::default(),
        }));

        let first = {
            let provisioner = provisioner.clone();
            tokio::spawn(async move { provisioner.provision().await })
        };

        // Wait until the first call is inside the runner, then ask again.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            provisioner.phase().is_some_and(|phase| phase.is_running()),
            "the first install should be in flight",
        );

        let second = provisioner.provision().await;
        assert_eq!(second, provisioner.phase().expect("phase retained"));

        gate.notify_one();
        let first_phase = first.await.expect("first task finished");

        // Two steps, once: the joined request did not start its own install.
        assert_eq!(
            inner.call_count(),
            2,
            "the join must not run the steps again"
        );
        assert_eq!(first_phase, InstallPhase::Complete { verified: true });
    }

    #[tokio::test]
    async fn a_request_after_completion_reinstalls() {
        // A finished operation is not a running one, so asking again is a
        // fresh attempt — which is what "re-try" means after a failure.
        let runner = ScriptedRunner::new(
            vec![fail_outcome("network down"), ok_outcome(), ok_outcome()],
            true,
        );
        let provisioner = provisioner(runner.clone(), true);

        assert!(!provisioner.provision().await.succeeded());
        assert!(provisioner.provision().await.succeeded());
        // Chromium is already present, so each attempt is the package step
        // alone: one failed, one successful.
        assert_eq!(runner.call_count(), 2);
    }

    #[tokio::test]
    async fn listeners_see_every_phase() {
        let sink = Arc::new(RecordingSink::default());

        let runner = ScriptedRunner::new(vec![ok_outcome(), ok_outcome()], true);
        let provisioner = provisioner(runner, false);
        provisioner.on_phase(sink.clone());

        provisioner.provision().await;

        let phases = sink.phases();
        assert!(phases.contains(&InstallPhase::Resolving));
        assert!(phases.contains(&InstallPhase::InstallingPackage));
        assert!(phases.contains(&InstallPhase::InstallingChromium));
        assert!(phases.contains(&InstallPhase::Verifying));
        assert_eq!(
            phases.last(),
            Some(&InstallPhase::Complete { verified: true })
        );
    }

    #[tokio::test]
    async fn the_final_phase_is_readable_afterwards() {
        // A settings pane opened mid-install reads this rather than guessing.
        let runner = ScriptedRunner::new(vec![fail_outcome("nope")], true);
        let provisioner = provisioner(runner, true);
        provisioner.provision().await;

        let phase = provisioner.phase().expect("phase is retained");
        assert!(!phase.is_running());
        assert!(!phase.succeeded());
    }

    /// A spawn failure names the program it could not start.
    ///
    /// "No such file or directory" alone does not say which of npm and npx was
    /// missing, and on a GUI-launched app's minimal PATH that is the whole
    /// question — the tool is installed, just not where this process looked.
    #[test]
    fn a_spawn_failure_names_the_program() {
        let outcome = CommandOutcome {
            success: false,
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            spawn_error: Some("npm: No such file or directory (os error 2)".to_string()),
            timed_out: false,
        };

        let detail = step_failure(&outcome).expect("a failure names a reason");
        assert!(detail.contains("npm"), "{detail}");
        assert!(detail.contains("No such file or directory"), "{detail}");
    }
}
