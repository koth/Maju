//! Installing and verifying the pinned browser provider.
//!
//! `BrowserFactory::acquire` spawns the provider's entry point from a
//! directory that nothing else creates. This module owns that directory's
//! layout, the install that populates it, and the check that says whether it
//! is usable — so the resolver, the verifier, preflight, and the spawn path
//! cannot disagree about where the provider lives.
//!
//! The package lives under Kodex's own data root rather than a global npm
//! prefix, so the version pinned in settings is the version that runs and a
//! user's global install cannot change which browser the agent gets.
//!
//! Nothing here reaches the network on its own. The install runs when the user
//! asks for it, because silently downloading tens of megabytes on someone's
//! behalf is not this feature's decision to make.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The provider package, as pinned in browser settings.
pub const PROVIDER_PACKAGE: &str = "@playwright/mcp";

/// Entry point inside an installed provider tree.
pub const PROVIDER_ENTRY: &str = "cli.js";

/// The provider version Kodex pins by default, re-exported from the model
/// crate so there is one number and not two that can drift.
pub use workspace_model::DEFAULT_BROWSER_PROVIDER_VERSION as DEFAULT_PROVIDER_VERSION;

/// Where everything the provider needs lives, given Kodex's data root.
///
/// Takes a plain root rather than `AppPaths`, because this crate sits below
/// the application layer.
pub fn install_root(data_root: &Path) -> PathBuf {
    data_root.join("browser")
}

/// The `npm` prefix for one provider version.
///
/// The *prefix* is versioned, not the package directory inside it: npm
/// resolves a `--prefix` by writing `<prefix>/node_modules/<package>`, so
/// versioning the prefix is what actually gives each version its own tree.
/// Versioning the package path instead would be silently overwritten by npm
/// writing to `<prefix>/node_modules/@playwright/mcp`.
pub fn version_prefix(data_root: &Path, provider_version: &str) -> PathBuf {
    install_root(data_root).join(format!("v{provider_version}"))
}

/// Root under which Kodex-owned browser profiles live.
///
/// Always inside Kodex's data root, never the user's real Chrome profile: a
/// persistent profile must survive sessions without any path existing that
/// could overwrite or read a real browser's data.
pub fn profiles_root(data_root: &Path) -> PathBuf {
    install_root(data_root).join("profiles")
}

/// The on-disk directory for one named profile.
pub fn profile_dir(data_root: &Path, profile_name: &str) -> PathBuf {
    profiles_root(data_root).join(profile_name)
}

/// Where npm places the package inside a version's prefix.
pub fn provider_package_root(data_root: &Path, provider_version: &str) -> PathBuf {
    version_prefix(data_root, provider_version)
        .join("node_modules")
        .join("@playwright")
        .join("mcp")
}

/// The file `BrowserFactory::acquire` actually spawns.
pub fn provider_entry_point(data_root: &Path, provider_version: &str) -> PathBuf {
    provider_package_root(data_root, provider_version).join(PROVIDER_ENTRY)
}

/// Everything one install needs to know about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProvider {
    pub package: String,
    /// Exact version, never a range: the pin is the point.
    pub version: String,
    /// npm `--prefix`.
    pub install_prefix: PathBuf,
    /// Directory this version installs into.
    pub package_root: PathBuf,
    /// The file that must exist for the provider to be considered installed.
    pub entry_point: PathBuf,
}

impl ResolvedProvider {
    pub fn resolve(data_root: &Path, provider_version: &str) -> Self {
        let version = provider_version.trim().to_string();
        let package_root = provider_package_root(data_root, &version);
        Self {
            package: PROVIDER_PACKAGE.to_string(),
            package_root: package_root.clone(),
            entry_point: package_root.join(PROVIDER_ENTRY),
            install_prefix: version_prefix(data_root, &version),
            version,
        }
    }

    /// The exact specifier handed to npm.
    pub fn specifier(&self) -> String {
        format!("{}@{}", self.package, self.version)
    }

    /// Arguments for `npm install`, split so a caller can log them safely.
    pub fn npm_install_args(&self) -> Vec<String> {
        vec![
            "install".to_string(),
            "--prefix".to_string(),
            self.install_prefix.to_string_lossy().into_owned(),
            "--no-save".to_string(),
            "--no-audit".to_string(),
            "--no-fund".to_string(),
            self.specifier(),
        ]
    }
}

/// What verification found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    /// Nothing installed at the resolved path.
    Absent,
    /// The tree exists and the entry point is there.
    Complete,
    /// The tree exists but the entry point is not there — a partial install, a
    /// wrong package name, or a write that stopped partway.
    ///
    /// Reported separately from `Absent` because the remedy differs: `Absent`
    /// wants an install, `Broken` wants a re-install *after* something
    /// explains why the tree is wrong.
    Broken,
}

impl InstallState {
    pub fn is_usable(self) -> bool {
        self == InstallState::Complete
    }

    /// Whether running the install is the right next step.
    pub fn wants_install(self) -> bool {
        self != InstallState::Complete
    }
}

/// Filesystem access, injected so verification is testable without a real
/// install tree.
pub trait ProvisionFs: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
}

/// The real filesystem.
pub struct HostFs;

impl ProvisionFs for HostFs {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
}

/// Decide whether a resolved provider is installed.
///
/// The check is entry-point existence and nothing more. Running the package to
/// read its version would execute provider code on every settings open, which
/// is a worse failure mode than a stale install that the next provision
/// replaces.
pub fn verify(resolved: &ResolvedProvider, fs: &dyn ProvisionFs) -> InstallState {
    if fs.exists(&resolved.entry_point) {
        return InstallState::Complete;
    }
    if fs.is_dir(&resolved.package_root) {
        // Something got written; it is just not a usable provider.
        return InstallState::Broken;
    }
    InstallState::Absent
}

/// Verify against the real filesystem.
pub fn verify_host(resolved: &ResolvedProvider) -> InstallState {
    verify(resolved, &HostFs)
}

/// Runs one command and reports how it went.
///
/// Abstracted so the install's command construction, failure mapping, and
/// timeout are all testable without a package manager, a network, or a
/// registry.
#[async_trait::async_trait]
pub trait CommandRunner: Send + Sync {
    /// Run `program` with `args`, streaming nothing, and return the outcome.
    /// `env` is the child's complete environment, not a patch.
    async fn run(
        &self,
        program: &str,
        args: &[String],
        env: &std::collections::HashMap<String, String>,
        timeout: Duration,
    ) -> CommandOutcome;
}

/// What running a command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub success: bool,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// The program could not be started at all.
    pub spawn_error: Option<String>,
    /// The command ran past its deadline and was abandoned.
    pub timed_out: bool,
}

impl CommandOutcome {
    /// The most useful line to quote back to the user.
    ///
    /// Package managers put the actual reason on stderr; falling back to
    /// stdout keeps a success-shaped message from being reported as a failure
    /// reason.
    pub fn failure_detail(&self) -> String {
        if let Some(error) = &self.spawn_error {
            return format!("无法启动安装程序：{error}");
        }
        if self.timed_out {
            // A download that stalls is almost never a slow network. It is a
            // connection that will never complete, which on a machine behind a
            // proxy in TUN mode means the traffic has no route. Naming the
            // likely cause is the difference between a user who knows what to
            // change and a user who retries.
            return "下载超时，连接一直没有完成。\
                    如果这台机器通过代理上网，请确认系统代理已开启，\
                    或设置 HTTPS_PROXY 环境变量后重试。"
                .to_string();
        }

        let stderr = last_meaningful_line(&self.stderr);
        if !stderr.is_empty() {
            return stderr;
        }
        let stdout = last_meaningful_line(&self.stdout);
        if !stdout.is_empty() {
            return stdout;
        }
        match self.status {
            Some(status) => format!("安装程序退出，状态码 {status}"),
            None => "安装失败".to_string(),
        }
    }
}

/// The first `node` on this process's `PATH`.
///
/// Only for tests in this crate: production resolves Node in `app-core`, which
/// reaches the same Homebrew and version-manager directories the app searches. A
/// test runs from a shell, so plain `PATH` is enough here.
#[cfg(test)]
fn host_node_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|dir| std::path::Path::new(dir).join("node"))
        .find(|candidate| candidate.is_file())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "node".to_string())
}

/// The reason a package manager failed, in one line.
///
/// npm prints its reason across two `npm error` lines — a machine code, then
/// the explanation — and closes with a pointer to a log file. The pointer is
/// the last line but the least useful: reporting it tells the user nothing
/// they can act on, so it is dropped and the code and explanation are joined.
fn last_meaningful_line(output: &str) -> String {
    let mut reasons: Vec<String> = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("npm warn"))
        // The log pointer is noise, not a reason.
        .filter(|line| !line.contains("A complete log of this run"))
        .filter_map(|line| line.strip_prefix("npm error").map(str::trim))
        .filter(|reason| !reason.is_empty())
        .map(str::to_string)
        .collect();

    if !reasons.is_empty() {
        // The first two carry the code and the explanation; the rest is usually
        // the "in most cases..." boilerplate.
        let head: Vec<&str> = reasons.iter().take(2).map(String::as_str).collect();
        return head.join(": ");
    }

    // A non-npm failure. The *first* stated error is the cause; the ones after
    // it are consequences — "Failed to install browsers", "Failed to download
    // X, caused by", "Download failure, code=1" — and the last of those is the
    // one with the least information in it.
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    let stated = lines.iter().find(|line| {
        !is_output_noise(line)
            && (line.starts_with("Error:") || line.contains("ENOTFOUND") || line.contains("code="))
    });
    stated
        .or_else(|| lines.iter().rev().find(|line| !is_output_noise(line)))
        .map(|line| line.to_string())
        .unwrap_or_default()
}

/// Lines that carry no reason, whatever tool produced them.
///
/// A fragment of a dumped object, a stack frame, a bare delimiter, or npm's
/// warning noise. Taking the last of these is what produced a failure reason of
/// `}` for a download that had actually said `ENOTFOUND` four lines earlier.
fn is_output_noise(line: &str) -> bool {
    if line.is_empty() || line.starts_with("at ") {
        return true;
    }
    if line.starts_with('{') || line.starts_with('}') || line.starts_with(']') {
        return true;
    }
    if line.starts_with("npm warn") || line.contains("A complete log of this run") {
        return true;
    }
    // The fields of a dumped error object, indented under it.
    ["errno:", "code:", "syscall:", "hostname:"]
        .iter()
        .any(|field| line.starts_with(field))
}

/// How long a package step may run before it is abandoned.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long the browser download may run.
///
/// Shorter than the package step, and deliberately so. The package step either
/// connects or reports why it could not; the download is the one that can sit
/// there for the full allowance making no progress, and the way it does that is
/// a connection to an address that black-holes it. That is what a proxy in TUN
/// mode looks like from Node when the proxy is not also exposed as a system
/// proxy: DNS answers with a synthetic address, nothing answers on it, and the
/// transfer waits forever rather than failing.
///
/// Ten silent minutes is a worse experience than a failure that names the
/// cause, and the cause here is always the same one worth naming.
pub const BROWSER_INSTALL_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// The environment an install runs in.
///
/// The parent's environment minus anything that would steer the package
/// manager or the provider. `PLAYWRIGHT_MCP_*` must not leak in: those are
/// provider runtime settings, and a stray one in a developer's shell would
/// otherwise change what gets installed.
///
/// The host's system proxy is added, because npm and Playwright read the proxy
/// from the environment only. A machine configured through System Settings has
/// it in neither, so the install fails to resolve a CDN the user reaches fine in
/// their own browser — and says `ENOTFOUND`, which reads like an outage rather
/// than a missing setting.
pub fn install_env(
    parent: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    let mut env: std::collections::HashMap<String, String> = parent
        .iter()
        .filter(|(key, _)| !key.to_ascii_uppercase().starts_with("PLAYWRIGHT_MCP_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    crate::proxy::apply_to(&mut env);
    env
}

/// The phase an install is in, pushed to the UI as it happens.
///
/// An install takes tens of seconds, so a settings pane that looks idle is
/// worse than one that says which step is running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum InstallPhase {
    Resolving,
    InstallingPackage,
    InstallingChromium,
    Verifying,
    /// Terminal.
    Complete {
        verified: bool,
    },
    Failed {
        step: String,
        detail: String,
    },
}

impl InstallPhase {
    /// Whether the operation is still doing work.
    pub fn is_running(&self) -> bool {
        !matches!(
            self,
            InstallPhase::Complete { .. } | InstallPhase::Failed { .. }
        )
    }

    /// Whether the operation ended in a usable provider.
    pub fn succeeded(&self) -> bool {
        matches!(self, InstallPhase::Complete { verified: true })
    }

    /// A short label for the settings pane.
    pub fn label(&self) -> String {
        match self {
            InstallPhase::Resolving => "正在检查所需环境…".to_string(),
            InstallPhase::InstallingPackage => "正在安装浏览器 provider…".to_string(),
            InstallPhase::InstallingChromium => "正在安装 Chromium…".to_string(),
            InstallPhase::Verifying => "正在校验安装结果…".to_string(),
            InstallPhase::Complete { verified: true } => "浏览器 provider 已就绪。".to_string(),
            InstallPhase::Complete { verified: false } => {
                "provider 已安装，但无法使用。".to_string()
            }
            InstallPhase::Failed { step, .. } => format!("{step} 失败。"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[derive(Default)]
    struct FakeFs {
        files: HashSet<PathBuf>,
        dirs: HashSet<PathBuf>,
    }

    impl FakeFs {
        fn with_file(mut self, path: &Path) -> Self {
            self.files.insert(path.to_path_buf());
            self
        }

        fn with_dir(mut self, path: &Path) -> Self {
            self.dirs.insert(path.to_path_buf());
            self
        }
    }

    impl ProvisionFs for FakeFs {
        fn exists(&self, path: &Path) -> bool {
            self.files.contains(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.dirs.contains(path)
        }
    }

    fn resolved() -> ResolvedProvider {
        ResolvedProvider::resolve(Path::new("/data"), DEFAULT_PROVIDER_VERSION)
    }

    #[test]
    fn the_entry_point_is_the_file_the_spawn_path_uses() {
        // `provider::build_launch` joins `cli.js` onto the package root it is
        // given. If these two ever disagree, the spawn fails at runtime and
        // preflight would still report the install as fine.
        let resolved = resolved();
        let via_resolver = resolved.entry_point.clone();
        let via_provider = crate::provider::provider_cli_from_package_root(&resolved.package_root);
        assert_eq!(via_resolver, via_provider);
    }

    #[test]
    fn the_package_lands_where_npm_writes_it() {
        // npm resolves `--prefix P` by writing `P/node_modules/<package>`.
        // The version has to be on the prefix, because versioning the path
        // *inside* node_modules would just be overwritten by the next install.
        let resolved = resolved();
        assert_eq!(
            resolved.package_root,
            resolved
                .install_prefix
                .join("node_modules")
                .join("@playwright")
                .join("mcp")
        );
        assert!(resolved.entry_point.starts_with(&resolved.install_prefix));
        // Each version still gets its own prefix.
        let other = ResolvedProvider::resolve(Path::new("/data"), "9.9.9");
        assert_ne!(resolved.install_prefix, other.install_prefix);
    }

    #[test]
    fn each_version_gets_its_own_prefix() {
        // A version change must not overwrite the tree a running session is
        // using, and it must not share a node_modules either.
        let a = ResolvedProvider::resolve(Path::new("/data"), "1.0.0");
        let b = ResolvedProvider::resolve(Path::new("/data"), "2.0.0");
        assert_ne!(a.install_prefix, b.install_prefix);
        assert_ne!(a.package_root, b.package_root);
        assert_ne!(a.entry_point, b.entry_point);
    }

    #[test]
    fn the_specifier_pins_the_exact_version() {
        assert_eq!(
            resolved().specifier(),
            format!("@playwright/mcp@{DEFAULT_PROVIDER_VERSION}")
        );
    }

    #[test]
    fn npm_args_target_the_prefix_and_avoid_writing_package_files() {
        let args = resolved().npm_install_args();
        assert_eq!(args.first().map(String::as_str), Some("install"));
        assert!(args.contains(&"--prefix".to_string()));
        assert!(
            args.iter().any(|arg| arg.starts_with("/data/browser/v")),
            "the prefix carries the version: {args:?}",
        );
        // No lockfile churn or registry chatter in an app-managed tree.
        assert!(args.contains(&"--no-save".to_string()));
        assert!(args.contains(&"--no-audit".to_string()));
        // Read from the resolver rather than a literal, so this test cannot
        // pass against a version the app would not actually install.
        assert_eq!(args.last().cloned(), Some(resolved().specifier()));
    }

    #[test]
    fn an_absent_provider_verifies_as_absent() {
        let resolved = resolved();
        assert_eq!(verify(&resolved, &FakeFs::default()), InstallState::Absent);
        assert!(InstallState::Absent.wants_install());
    }

    #[test]
    fn a_complete_provider_verifies_as_complete() {
        let resolved = resolved();
        let fs = FakeFs::default().with_file(&resolved.entry_point);
        assert_eq!(verify(&resolved, &fs), InstallState::Complete);
        assert!(InstallState::Complete.is_usable());
        // Nothing to do, so no install should be offered.
        assert!(!InstallState::Complete.wants_install());
    }

    #[test]
    fn a_tree_without_the_entry_point_is_broken_not_absent() {
        // The directory exists, so something was written. Reporting "absent"
        // would send the user to install again with no explanation for why the
        // same thing keeps happening.
        let resolved = resolved();
        let fs = FakeFs::default().with_dir(&resolved.package_root);
        assert_eq!(verify(&resolved, &fs), InstallState::Broken);
        assert!(InstallState::Broken.wants_install());
    }

    #[test]
    fn the_entry_point_wins_even_when_the_directory_is_also_listed() {
        let resolved = resolved();
        let fs = FakeFs::default()
            .with_dir(&resolved.package_root)
            .with_file(&resolved.entry_point);
        assert_eq!(verify(&resolved, &fs), InstallState::Complete);
    }

    #[test]
    fn an_empty_version_still_resolves_rather_than_panicking() {
        // Preflight validates the version upstream, but a resolver that panics
        // on odd input takes the settings pane down with it.
        let resolved = ResolvedProvider::resolve(Path::new("/data"), "");
        assert!(resolved.package_root.to_string_lossy().contains("v"));
    }

    #[test]
    fn install_phases_report_whether_they_are_running_and_whether_they_succeeded() {
        assert!(InstallPhase::Resolving.is_running());
        assert!(InstallPhase::InstallingChromium.is_running());
        assert!(!InstallPhase::Verifying.succeeded());

        let done = InstallPhase::Complete { verified: true };
        assert!(!done.is_running());
        assert!(done.succeeded());

        let unusable = InstallPhase::Complete { verified: false };
        assert!(!unusable.is_running());
        assert!(
            !unusable.succeeded(),
            "installed but unusable is not success"
        );

        let failed = InstallPhase::Failed {
            step: "正在安装浏览器 provider".to_string(),
            detail: "registry unreachable".to_string(),
        };
        assert!(!failed.is_running());
        assert!(!failed.succeeded());
        assert!(failed.label().contains("失败"));
    }

    #[test]
    fn install_env_drops_provider_runtime_overrides() {
        let parent: std::collections::HashMap<String, String> = [
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("PLAYWRIGHT_MCP_BROWSER".to_string(), "firefox".to_string()),
            ("playwright_mcp_headless".to_string(), "false".to_string()),
            (
                "NPM_CONFIG_REGISTRY".to_string(),
                "https://example.test".to_string(),
            ),
        ]
        .into_iter()
        .collect();

        let env = install_env(&parent);

        assert!(env.contains_key("PATH"));
        // A developer's registry override is honoured: it is how an internal
        // mirror is pointed at, not a provider runtime setting.
        assert!(env.contains_key("NPM_CONFIG_REGISTRY"));
        assert!(!env.contains_key("PLAYWRIGHT_MCP_BROWSER"));
        assert!(!env.contains_key("playwright_mcp_headless"));
    }

    #[test]
    fn a_failure_quotes_what_the_installer_actually_said() {
        // npm interleaves progress with the real error, and the error is what
        // the user needs; quoting the progress line would be useless.
        let outcome = CommandOutcome {
            success: false,
            status: Some(1),
            stdout: "npm warn deprecated something\n".to_string(),
            stderr: "npm error code E404\nnpm error 404 Not Found - GET https://registry.npmjs.org/@playwright%2fmcp\n".to_string(),
            spawn_error: None,
            timed_out: false,
        };

        let detail = outcome.failure_detail();
        assert!(detail.contains("404"), "got {detail}");
        // The trailing error line, not the deprecation warning above it.
        assert!(detail.contains("Not Found"), "got {detail}");
    }

    #[test]
    fn a_missing_installer_is_reported_as_missing_not_as_a_failed_install() {
        // Different remedy: install npm, versus fix the network.
        let outcome = CommandOutcome {
            success: false,
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            spawn_error: Some("No such file or directory (os error 2)".to_string()),
            timed_out: false,
        };
        assert!(outcome.failure_detail().contains("无法启动安装程序"));
    }

    #[test]
    fn a_timeout_is_its_own_reason() {
        let outcome = CommandOutcome {
            success: false,
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            spawn_error: None,
            timed_out: true,
        };
        let detail = outcome.failure_detail();
        assert!(detail.contains("超时"), "{detail}");
        // It must say what to do, not only that time ran out. A stalled
        // download on a proxied machine is a routing problem, and a user told
        // only "timed out" retries.
        assert!(detail.contains("HTTPS_PROXY"), "{detail}");
    }

    #[test]
    fn a_silent_failure_still_produces_a_reason() {
        let outcome = CommandOutcome {
            success: false,
            status: Some(3),
            stdout: String::new(),
            stderr: "   \n".to_string(),
            spawn_error: None,
            timed_out: false,
        };
        assert!(outcome.failure_detail().contains("状态码 3"));
    }

    #[test]
    fn the_install_timeout_is_generous_but_finite() {
        // An install that hangs must not hold the UI open forever, and a
        // real one over a slow link still has to fit.
        assert!(INSTALL_TIMEOUT >= Duration::from_secs(120));
        assert!(INSTALL_TIMEOUT <= Duration::from_secs(30 * 60));
    }

    /// A real install, skipped unless explicitly requested.
    ///
    /// The only test anywhere that touches npm or the network. It is what
    /// proves the argument construction, the prefix layout, and the
    /// verification step all agree with a real package manager — everything
    /// else is a scripted double.
    #[test]
    #[ignore = "reaches the npm registry; run explicitly"]
    fn real_install_smoke() {
        // Read the version from the settings default rather than a literal: a
        // literal here would let this test pass against a version the app
        // would never install.
        let pinned = workspace_model::BrowserSettings::default().provider_version;

        let dir =
            std::env::temp_dir().join(format!("kodex-provision-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // Read from the settings default rather than a literal, so this test
        // cannot pass against a version the app would not actually install.
        let resolved = ResolvedProvider::resolve(&dir, &pinned);
        assert_eq!(
            verify_host(&resolved),
            InstallState::Absent,
            "nothing is installed to begin with"
        );

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let deps = crate::installer::ProvisionDeps {
            resolved: resolved.clone(),
            runner: std::sync::Arc::new(crate::installer::HostCommandRunner),
            fs: std::sync::Arc::new(HostFs),
            npm: "npm".to_string(),
            npx: "npx".to_string(),
            node: host_node_path(),
            // Set from the environment so the same test can exercise the
            // package step alone, or both steps. The Chromium download is
            // hundreds of megabytes, so it is opt-in.
            chromium_installed: std::env::var_os("KODEX_SMOKE_SKIP_CHROMIUM").is_some(),
            parent_env: std::env::vars().collect(),
        };

        let phase = runtime.block_on(crate::installer::Provisioner::new(deps).provision());
        eprintln!("install phase: {phase:?}");

        assert_eq!(verify_host(&resolved), InstallState::Complete, "{phase:?}");
        assert!(resolved.entry_point.is_file(), "entry point is missing");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_phase_serialises_to_a_tagged_shape() {
        // The UI reads this, so the wire shape is part of the contract.
        let json = serde_json::to_value(InstallPhase::InstallingPackage).unwrap();
        assert_eq!(json["phase"], "installingPackage");
        let parsed: InstallPhase = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, InstallPhase::InstallingPackage);
    }

    /// A real Playwright download failure, captured verbatim.
    ///
    /// The tail of that output is `}`: a dumped error object's closing brace,
    /// after a stack frame. Taking the last line — which is what this used to
    /// do for anything that is not npm — reported the reason as `}`, the same
    /// uselessness as npm's log pointer one level down.
    #[test]
    fn a_download_failure_reports_the_reason_not_the_closing_brace() {
        let output = "\
Downloading Chrome for Testing 154.0.8037.0 (playwright chromium v1246) from https://cdn.playwright.dev/builds/cft/154.0.8037.0/mac-arm64/chrome-mac-arm64.zip
Error: getaddrinfo ENOTFOUND cdn.playwright.dev
    at GetAddrInfoReqWrap.onlookupall [as oncomplete] (node:internal/dns/promises:101:17) {
  errno: -3008,
  code: 'ENOTFOUND',
  syscall: 'getaddrinfo',
  hostname: 'cdn.playwright.dev'
}
Failed to install browsers
Error: Failed to download Chrome for Testing 154.0.8037.0 (playwright chromium v1246), caused by
Error: Download failure, code=1
    at ChildProcess.<anonymous> (/x/playwright-core/lib/coreBundle.js:33150:32)
";

        let detail = last_meaningful_line(output);
        // The cause, not the last of the consequences.
        assert_eq!(
            detail, "Error: getaddrinfo ENOTFOUND cdn.playwright.dev",
            "the DNS failure is the actionable line"
        );
    }

    #[test]
    fn a_stack_frame_is_never_reported_as_the_reason() {
        let output = "Something went wrong\n    at run (/x/y.js:1:1)\n    at other (/x/z.js:2:2)\n";
        let detail = last_meaningful_line(output);
        assert_eq!(detail, "Something went wrong");
    }

    #[test]
    fn a_dumped_error_object_is_never_reported_as_the_reason() {
        let output = "could not do the thing\n  errno: -3008,\n  code: 'ENOTFOUND',\n  syscall: 'getaddrinfo',\n}\n";
        let detail = last_meaningful_line(output);
        assert_eq!(detail, "could not do the thing");
    }
}
