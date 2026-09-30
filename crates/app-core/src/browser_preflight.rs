//! Preflight checks that decide whether browser-use and computer-use can run
//! on this host, before any session is given the tools.
//!
//! Both capabilities are opt-in and fail closed: when preflight cannot prove
//! the dependency is usable, the capability stays disabled and the reason is
//! surfaced to settings and to the session that asked for it. A half-working
//! browser tool that silently fails mid-turn is worse than a clear "not
//! configured" state.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use workspace_model::{BrowserMode, BrowserSettings, ComputerUseSettings, PreflightFix};

/// The DTO the checks produce, and the only definition of it.
///
/// This used to be a second, near-identical enum declared here, converted
/// field by field into `workspace_model`'s copy. Two definitions of one wire
/// type is how the checks ended up disagreeing with the DTO about what to
/// serialize, so the conversion is gone rather than kept in sync.
pub use workspace_model::PreflightState;

/// A dependency with no remedy Kodex can apply, such as a missing Node runtime.
fn missing(detail: impl Into<String>, remedy: impl Into<String>) -> PreflightState {
    PreflightState::Missing {
        detail: detail.into(),
        remedy: remedy.into(),
        fix: PreflightFix::Configure,
    }
}

/// A dependency the installer can supply.
fn installable(detail: impl Into<String>, remedy: impl Into<String>) -> PreflightState {
    PreflightState::Missing {
        detail: detail.into(),
        remedy: remedy.into(),
        fix: PreflightFix::Install,
    }
}

/// A configuration the user has to change.
fn misconfigured(detail: impl Into<String>, remedy: impl Into<String>) -> PreflightState {
    PreflightState::Invalid {
        detail: detail.into(),
        remedy: remedy.into(),
        fix: PreflightFix::Configure,
    }
}

/// Browser preflight result, surfaced in settings and per session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserPreflight {
    pub state: PreflightState,
    /// Absolute path of the Node executable that would launch the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_executable: Option<PathBuf>,
    /// Provider version Kodex will pin, as configured.
    pub provider_version: String,
}

/// Computer-use preflight result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ComputerPreflight {
    pub state: PreflightState,
    /// Whether the host platform is one the Cua Driver supports.
    pub platform_supported: bool,
}

/// Host facts preflight needs, injected so the checks stay testable without
/// touching the real filesystem or the real PATH.
pub trait PreflightEnvironment {
    /// Resolve an executable by name, as `PATH` lookup would.
    fn resolve_executable(&self, name: &str) -> Option<PathBuf>;
    /// Whether the pinned provider package is installed under Kodex's data
    /// root. Checked separately from the browser install because the two fail
    /// for different reasons and have different remedies.
    fn provider_package_installed(&self, provider_version: &str) -> bool;
    /// Whether a path exists and is a file.
    fn is_file(&self, path: &Path) -> bool;
    /// Whether a path exists and is a directory.
    fn is_dir(&self, path: &Path) -> bool;
    /// Entry names directly inside a directory, empty when unreadable.
    ///
    /// Injected rather than read inline because the browser check is decided by
    /// *which* entry is present, and a test that cannot say which entries exist
    /// cannot express the case that actually broke: a host with the wrong
    /// Chromium build.
    fn dir_entries(&self, path: &Path) -> Vec<String>;
    /// The Chromium revision the pinned provider will launch, or `None` when
    /// there is no provider or no registry to read it from.
    fn provider_browser_revision(&self) -> Option<String>;
}

/// Real host environment.
pub struct HostEnvironment;

impl PreflightEnvironment for HostEnvironment {
    fn resolve_executable(&self, name: &str) -> Option<PathBuf> {
        // The same search the app uses for its agent CLIs: PATH plus Homebrew
        // plus the common version managers. A plain PATH lookup reports Node as
        // missing on a GUI-launched app, which inherits a PATH that omits
        // all of them, even though the user plainly has it installed.
        dsh_bridge::find_binary(name)
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn dir_entries(&self, path: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect()
    }

    fn provider_package_installed(&self, provider_version: &str) -> bool {
        let Some(paths) = crate::AppPaths::resolve().ok() else {
            return false;
        };
        provider_package_root(&paths, provider_version)
            .join("cli.js")
            .is_file()
    }

    fn provider_browser_revision(&self) -> Option<String> {
        let paths = crate::AppPaths::resolve().ok()?;
        let version = crate::settings::load_app_settings(&paths)
            .browser
            .provider_version;
        host_provider_browser_revision(&provider_package_root(&paths, &version))
    }
}

/// Playwright's own browser registry, inside the installed provider's tree.
fn host_provider_browser_revision(package_root: &Path) -> Option<String> {
    // node_modules/@playwright/mcp -> node_modules/@playwright -> node_modules
    let registry = package_root
        .parent()?
        .parent()?
        .join("playwright-core")
        .join("browsers.json");
    let raw = std::fs::read_to_string(registry).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    parsed
        .get("browsers")?
        .as_array()?
        .iter()
        // The `chromium` entry's revision is what the `chrome-for-testing`
        // build follows; the headless shell carries its own.
        .find(|browser| browser.get("name").and_then(serde_json::Value::as_str) == Some("chromium"))
        .and_then(|browser| browser.get("revision"))
        .and_then(|revision| match revision {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
}

/// Where the pinned provider is expected to be installed.
///
/// Mirrors the layout `SharedMcpServers::browser_server` launches from, so
/// preflight and the spawn path cannot disagree about the path.
pub fn provider_package_root(paths: &crate::AppPaths, provider_version: &str) -> PathBuf {
    // One resolver, owned by the crate that spawns the provider. Preflight,
    // the provisioner, and `BrowserFactory` all resolve through it, so they
    // cannot disagree about where the provider lives.
    browser_service::provision::provider_package_root(paths.root(), provider_version)
}

/// Where Playwright keeps its downloaded browsers, per platform.
fn playwright_browsers_root() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH")
        && !explicit.is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let cache = if cfg!(target_os = "macos") {
        home.join("Library").join("Caches")
    } else if cfg!(target_os = "windows") {
        home.join("AppData").join("Local")
    } else {
        home.join(".cache")
    };
    Some(cache.join("ms-playwright"))
}

/// Check whether a Playwright browser install exists, on the real host.
pub fn playwright_browser_installed_on_host() -> bool {
    playwright_browser_installed(&HostEnvironment)
}

/// Check whether a Playwright browser install exists.
///
/// This checks for the revision the installed provider actually wants, not for
/// any `chromium*` entry. The looser check was wrong in a way only a host that
/// had *some other* Chromium could show: preflight reported ready, the install
/// reported success, and the provider then refused to start with its own
/// `Browser "chrome-for-testing" is not installed` error. A host missing the
/// right build is precisely what this function exists to catch.
pub fn playwright_browser_installed(env: &dyn PreflightEnvironment) -> bool {
    let Some(root) = playwright_browsers_root() else {
        return false;
    };
    if !env.is_dir(&root) {
        return false;
    }
    // The revision the *installed provider* will launch, read from Playwright's
    // own registry rather than guessed.
    let Some(revision) = env.provider_browser_revision() else {
        // No provider, or no registry to read a revision from. Guessing is what
        // this check used to do, and it is why a wrong build passed.
        return false;
    };
    env.dir_entries(&root)
        .iter()
        .any(|name| name == &format!("chromium-{revision}"))
}

/// Resolve the browser preflight result.
///
/// Order matters: configuration validity is checked before the environment, so
/// a user who selected attach mode without an endpoint is told that rather
/// than being sent to install a browser.
pub fn check_browser(
    settings: &BrowserSettings,
    env: &dyn PreflightEnvironment,
) -> BrowserPreflight {
    check_browser_with(settings, env, crate::panel_browser::endpoint().as_deref())
}

/// [`check_browser`], with the panel browser named rather than looked up.
///
/// `panel_endpoint` is the DevTools endpoint of the browser the right panel is
/// showing, when one is running. It decides one thing and one thing only: which
/// browser the tools drive. Playwright attaches to that browser over CDP instead
/// of launching one, so no Chromium has to be on the machine at all — while Node
/// and the pinned provider package are still required, because the tools still
/// run as a provider process.
///
/// Split from [`check_browser`] so the decision can be tested against a named
/// endpoint instead of whatever the running app has published.
pub fn check_browser_with(
    settings: &BrowserSettings,
    env: &dyn PreflightEnvironment,
    panel_endpoint: Option<&str>,
) -> BrowserPreflight {
    let provider_version = settings.provider_version.clone();

    if let Err(detail) = settings.validate() {
        return BrowserPreflight {
            state: misconfigured("Fix the browser settings, then re-run preflight.", detail),
            node_executable: None,
            provider_version,
        };
    }

    // Attach mode talks to a browser the user already runs, so it needs no
    // local install check — only the endpoint, already validated above.
    //
    // Persistent mode does need the local browser: it is Kodex's own Chromium
    // with Kodex's own profile. Only the profile directory differs.
    if settings.mode == BrowserMode::Attach {
        return BrowserPreflight {
            state: PreflightState::Ready,
            node_executable: None,
            provider_version,
        };
    }

    let Some(node) = env.resolve_executable("node") else {
        return BrowserPreflight {
            state: missing(
                "未找到 Node.js。",
                "Kodex 需要 Node.js 18 或更高版本才能启动浏览器 provider。",
            ),
            node_executable: None,
            provider_version,
        };
    };

    // The provider package is provisioned into Kodex's data root. Without it
    // every tool call would fail at spawn time, so it is checked here rather
    // than discovered by the user on their first browser tool call.
    if !env.provider_package_installed(&settings.provider_version) {
        return BrowserPreflight {
            // `installable`, not `missing`: this is precisely the dependency the
            // provisioner can supply. Reporting it as `Configure` would hide the
            // one-click install behind a remedy the user has to run by hand.
            state: installable(
                format!(
                    "尚未安装固定版本的浏览器 provider（v{}）。",
                    settings.provider_version
                ),
                "Kodex 可以为你一键安装，也可以自行运行 `npm install`。".to_string(),
            ),
            node_executable: Some(node),
            provider_version,
        };
    }

    // The panel's browser is the one these tools drive, so there is nothing to
    // launch and nothing to install — not even the executable these settings
    // name, which is inert while the panel is the browser. This is the only
    // requirement a running panel removes; Node and the provider package above
    // are still what runs the tools.
    if panel_endpoint.is_some() {
        return BrowserPreflight {
            state: PreflightState::Ready,
            node_executable: Some(node),
            provider_version,
        };
    }

    if let Some(explicit) = (!settings.executable_path.trim().is_empty())
        .then(|| PathBuf::from(settings.executable_path.trim()))
    {
        if !env.is_file(&explicit) {
            return BrowserPreflight {
                state: misconfigured(
                    format!(
                        "Configured browser executable does not exist: {}",
                        explicit.display()
                    ),
                    "Point executablePath at a Chromium binary, or clear it to use provider discovery.",
                ),
                node_executable: Some(node),
                provider_version,
            };
        }
    } else if !playwright_browser_installed(env) {
        return BrowserPreflight {
            // The provisioner downloads Chromium as its own phase, so this is
            // installable too — sending the user to a terminal instead would
            // leave the app's own install path half-used.
            state: installable(
                "未找到这个 provider 版本需要的 Chromium。",
                "Kodex 可以为你一键安装；必须安装 provider 自己指定的那一版，\
                 缓存里其它版本的 Chromium 它不会使用。",
            ),
            node_executable: Some(node),
            provider_version,
        };
    }

    BrowserPreflight {
        state: PreflightState::Ready,
        node_executable: Some(node),
        provider_version,
    }
}

/// Platforms the Cua Driver native SDK ships for.
fn platform_supported() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    ))
}

/// Resolve the computer-use preflight result.
///
/// The driver is embedded rather than spawned, so there is no PATH lookup to
/// perform; what can fail is the platform and the configuration.
pub fn check_computer_use(settings: &ComputerUseSettings) -> ComputerPreflight {
    if let Err(detail) = settings.validate() {
        return ComputerPreflight {
            state: misconfigured(
                "Fix the computer-use settings, then re-run preflight.",
                detail,
            ),
            platform_supported: platform_supported(),
        };
    }

    if !platform_supported() {
        return ComputerPreflight {
            state: PreflightState::Missing {
                detail: format!("The Cua Driver has no build for {}.", std::env::consts::OS),
                remedy: "Use browser-use instead, or run Kodex on macOS, Windows, or Linux."
                    .to_string(),
                fix: PreflightFix::Configure,
            },
            platform_supported: false,
        };
    }

    ComputerPreflight {
        state: PreflightState::Ready,
        platform_supported: true,
    }
}

/// Whether a capability should be injected into a session, given settings and
/// preflight. Disabled settings and failed preflight both withhold the tools,
/// and both carry a reason the UI can show.
pub fn browser_injection_decision(
    settings: &BrowserSettings,
    preflight: &BrowserPreflight,
) -> BrowserInjectionDecision {
    if !settings.enabled {
        return BrowserInjectionDecision::Disabled;
    }
    if preflight.state.is_ready() {
        BrowserInjectionDecision::Inject
    } else {
        BrowserInjectionDecision::Unavailable(preflight.state.clone())
    }
}

pub fn computer_injection_decision(
    settings: &ComputerUseSettings,
    preflight: &ComputerPreflight,
) -> ComputerInjectionDecision {
    if !settings.enabled {
        return ComputerInjectionDecision::Disabled;
    }
    if preflight.state.is_ready() {
        ComputerInjectionDecision::Inject
    } else {
        ComputerInjectionDecision::Unavailable(preflight.state.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserInjectionDecision {
    Disabled,
    Inject,
    Unavailable(PreflightState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerInjectionDecision {
    Disabled,
    Inject,
    Unavailable(PreflightState),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeEnv {
        executables: HashMap<String, PathBuf>,
        files: Vec<PathBuf>,
        dirs: Vec<PathBuf>,
        /// Whether the pinned provider package is installed. On by default so
        /// the other tests are not all about a missing package.
        provider_package_installed: bool,
        /// The revision the pinned provider wants, or `None` for no provider.
        revision: Option<String>,
        /// Entry names in the Playwright cache.
        entries: Vec<String>,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self {
                executables: HashMap::new(),
                files: Vec::new(),
                dirs: Vec::new(),
                provider_package_installed: true,
                revision: Some("1246".to_string()),
                entries: Vec::new(),
            }
        }

        fn with_node(mut self) -> Self {
            self.executables
                .insert("node".to_string(), PathBuf::from("/usr/bin/node"));
            self
        }

        fn with_file(mut self, path: &str) -> Self {
            self.files.push(PathBuf::from(path));
            self
        }

        fn with_dir(mut self, path: &str) -> Self {
            self.dirs.push(PathBuf::from(path));
            self
        }

        fn without_provider(mut self) -> Self {
            self.provider_package_installed = false;
            self.revision = None;
            self
        }

        /// A host with a cache directory holding the *wrong* build.
        fn with_other_chromium_build(mut self) -> Self {
            self.dirs
                .push(playwright_browsers_root().expect("playwright cache"));
            self.entries = vec![
                "chromium-1208".to_string(),
                "chromium-1243".to_string(),
                "ffmpeg-1011".to_string(),
            ];
            self
        }

        /// A host whose cache also holds the exact build the provider wants.
        ///
        /// Differs from [`FakeEnv::with_other_chromium_build`] only in the last
        /// entry, so the two tests between them isolate the revision and nothing
        /// else.
        fn with_provider_chromium_build(mut self) -> Self {
            self.dirs
                .push(playwright_browsers_root().expect("playwright cache"));
            self.entries = vec!["chromium-1243".to_string(), "chromium-1246".to_string()];
            self
        }
    }

    impl PreflightEnvironment for FakeEnv {
        fn resolve_executable(&self, name: &str) -> Option<PathBuf> {
            self.executables.get(name).cloned()
        }

        fn is_file(&self, path: &Path) -> bool {
            self.files.contains(&path.to_path_buf())
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.dirs.contains(&path.to_path_buf())
        }

        fn dir_entries(&self, _path: &Path) -> Vec<String> {
            self.entries.clone()
        }

        fn provider_package_installed(&self, _provider_version: &str) -> bool {
            self.provider_package_installed
        }

        fn provider_browser_revision(&self) -> Option<String> {
            self.revision.clone()
        }
    }

    #[test]
    fn disabled_browser_settings_are_the_default_and_never_inject() {
        let settings = BrowserSettings::default();
        assert!(!settings.enabled, "browser-use must default to disabled");
        assert!(!settings.allow_attach, "attach must default to off");

        let preflight = check_browser(&settings, &FakeEnv::new().with_node());
        assert_eq!(
            browser_injection_decision(&settings, &preflight),
            BrowserInjectionDecision::Disabled
        );
    }

    #[test]
    fn attach_mode_without_endpoint_is_invalid_before_any_env_check() {
        let settings = BrowserSettings {
            enabled: true,
            mode: BrowserMode::Attach,
            endpoint: String::new(),
            allow_attach: true,
            ..BrowserSettings::default()
        };

        // No node and no browser install on the fake host: the configuration
        // error must still win, so the user is not sent to install anything.
        let preflight = check_browser(&settings, &FakeEnv::new());
        assert!(matches!(preflight.state, PreflightState::Invalid { .. }));
    }

    #[test]
    fn attach_mode_requires_explicit_opt_in() {
        let settings = BrowserSettings {
            enabled: true,
            mode: BrowserMode::Attach,
            endpoint: "http://127.0.0.1:9222".to_string(),
            allow_attach: false,
            ..BrowserSettings::default()
        };

        let preflight = check_browser(&settings, &FakeEnv::new());
        assert!(matches!(preflight.state, PreflightState::Invalid { .. }));
    }

    #[test]
    fn attach_mode_needs_no_local_browser_install() {
        let settings = BrowserSettings {
            enabled: true,
            mode: BrowserMode::Attach,
            endpoint: "http://127.0.0.1:9222".to_string(),
            allow_attach: true,
            ..BrowserSettings::default()
        };

        // No node, no Chromium: an attached browser is the user's own process.
        let preflight = check_browser(&settings, &FakeEnv::new());
        assert!(preflight.state.is_ready(), "got {:?}", preflight.state);
    }

    #[test]
    fn a_missing_provider_package_is_reported_before_the_browser_install() {
        // The provider package is what actually gets spawned. Checking the
        // browser install first would send the user to `playwright install`
        // when the real problem is an unprovisioned package.
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };
        let preflight = check_browser(&settings, &FakeEnv::new().with_node().without_provider());

        match &preflight.state {
            PreflightState::Missing {
                detail,
                remedy,
                fix,
            } => {
                assert!(detail.contains("浏览器 provider"), "got {detail}");
                assert!(remedy.contains("npm install"), "got {remedy}");
                // The one case that must offer the button: this is the
                // dependency the provisioner exists to supply.
                assert_eq!(
                    *fix,
                    PreflightFix::Install,
                    "a missing provider package must offer the install action"
                );
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn attach_mode_does_not_need_the_local_provider_package() {
        // Attaching talks to a browser the user already runs; no provider
        // process is started, so nothing needs installing.
        let settings = BrowserSettings {
            enabled: true,
            mode: BrowserMode::Attach,
            endpoint: "http://127.0.0.1:9222".to_string(),
            allow_attach: true,
            ..BrowserSettings::default()
        };
        let preflight = check_browser(&settings, &FakeEnv::new().without_provider());
        assert!(preflight.state.is_ready(), "got {:?}", preflight.state);
    }

    #[test]
    fn launch_mode_without_node_reports_missing_with_remedy() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };

        let preflight = check_browser(&settings, &FakeEnv::new());
        match &preflight.state {
            PreflightState::Missing {
                detail,
                remedy,
                fix,
            } => {
                assert!(detail.contains("Node.js"), "got {detail}");
                assert!(remedy.contains("Node.js"), "got {remedy}");
                // Installing the provider cannot conjure a Node runtime, so this
                // one is a configuration problem, not an install.
                assert_eq!(
                    *fix,
                    PreflightFix::Configure,
                    "a missing Node runtime must not offer the install action"
                );
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn launch_mode_with_node_but_no_browser_reports_the_install_command() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };

        let preflight = check_browser(&settings, &FakeEnv::new().with_node());
        match &preflight.state {
            PreflightState::Missing { detail, fix, .. } => {
                assert!(detail.contains("Chromium"), "got {detail}");
                assert_eq!(
                    *fix,
                    PreflightFix::Install,
                    "the provisioner downloads Chromium, so this is installable"
                );
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    /// A host with the *wrong* Chromium build is not a ready host.
    ///
    /// This is the case that actually happened: the machine had `chromium-1243`
    /// from an unrelated `npx playwright install`, the provider wanted
    /// `chromium-1246` (its `chrome-for-testing` build), and a check for "any
    /// `chromium*`" called the host ready. The install then reported success
    /// and the first tool call failed with the provider's own
    /// `Browser "chrome-for-testing" is not installed`.
    #[test]
    fn a_different_chromium_build_does_not_count_as_installed() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };
        let env = FakeEnv::new().with_node().with_other_chromium_build();

        let preflight = check_browser(&settings, &env);

        match &preflight.state {
            PreflightState::Missing { detail, fix, .. } => {
                assert!(detail.contains("Chromium"), "got {detail}");
                assert_eq!(
                    *fix,
                    PreflightFix::Install,
                    "the exact build is installable, so the pane must offer it"
                );
            }
            other => panic!(
                "a host with chromium-1243 cannot serve a provider that wants 1246: {other:?}"
            ),
        }
    }

    #[test]
    fn the_exact_build_the_provider_wants_counts_as_installed() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };
        let env = FakeEnv::new().with_node().with_provider_chromium_build();

        let preflight = check_browser(&settings, &env);
        assert!(preflight.state.is_ready(), "got {:?}", preflight.state);
    }

    /// Every outcome `check_browser` can produce, with the `fix` it advertises.
    ///
    /// This table is the contract the settings pane's install button depends on.
    /// Asserting it in one place means a new branch — or an edited one — cannot
    /// quietly flip an installable dependency back to `Configure` and hide the
    /// button, which is exactly the bug a prose-level assertion misses.
    #[test]
    fn every_browser_preflight_outcome_advertises_the_right_fix() {
        let enabled = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };
        let attach = BrowserSettings {
            enabled: true,
            mode: BrowserMode::Attach,
            endpoint: "http://127.0.0.1:9222".to_string(),
            allow_attach: true,
            ..BrowserSettings::default()
        };
        let bad_executable = BrowserSettings {
            enabled: true,
            executable_path: "/nope/chromium".to_string(),
            ..BrowserSettings::default()
        };

        struct Case {
            what: &'static str,
            state: PreflightState,
            expected: PreflightFix,
        }
        let fix_of = |state: &PreflightState| match state {
            PreflightState::Ready => panic!("ready has no fix: {state:?}"),
            PreflightState::Missing { fix, .. } | PreflightState::Invalid { fix, .. } => *fix,
        };

        let cases = vec![
            Case {
                what: "no node",
                state: check_browser(&enabled, &FakeEnv::new()).state,
                expected: PreflightFix::Configure,
            },
            Case {
                what: "node but no provider package",
                state: check_browser(&enabled, &FakeEnv::new().with_node().without_provider())
                    .state,
                expected: PreflightFix::Install,
            },
            Case {
                what: "node and provider but no chromium",
                state: check_browser(&enabled, &FakeEnv::new().with_node()).state,
                expected: PreflightFix::Install,
            },
            Case {
                what: "executable path that does not exist",
                state: check_browser(&bad_executable, &FakeEnv::new().with_node()).state,
                expected: PreflightFix::Configure,
            },
            Case {
                what: "attach without opt-in",
                state: check_browser(
                    &BrowserSettings {
                        allow_attach: false,
                        ..attach.clone()
                    },
                    &FakeEnv::new(),
                )
                .state,
                expected: PreflightFix::Configure,
            },
        ];

        for case in cases {
            assert_eq!(
                fix_of(&case.state),
                case.expected,
                "{}: fix does not match the remedy it advertises",
                case.what
            );
        }

        // The two attach rows above are the configuration gate; attach itself,
        // once opted in, is Ready and therefore carries no fix at all.
        assert!(check_browser(&attach, &FakeEnv::new()).state.is_ready());
    }

    #[test]
    fn explicit_executable_path_skips_the_install_check() {
        let settings = BrowserSettings {
            enabled: true,
            executable_path: "/Applications/Chromium.app/Contents/MacOS/Chromium".to_string(),
            ..BrowserSettings::default()
        };

        let env = FakeEnv::new()
            .with_node()
            .with_file("/Applications/Chromium.app/Contents/MacOS/Chromium");

        let preflight = check_browser(&settings, &env);
        assert!(preflight.state.is_ready(), "got {:?}", preflight.state);
    }

    #[test]
    fn missing_explicit_executable_is_invalid_not_missing() {
        let settings = BrowserSettings {
            enabled: true,
            executable_path: "/nope/chromium".to_string(),
            ..BrowserSettings::default()
        };

        let preflight = check_browser(&settings, &FakeEnv::new().with_node());
        assert!(matches!(preflight.state, PreflightState::Invalid { .. }));
    }

    #[test]
    fn a_running_panel_browser_needs_no_chromium_of_its_own() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };
        // Node and the provider package, and nothing else: no executable path,
        // no Chromium in the provider's cache.
        let env = FakeEnv::new().with_node();

        let without = check_browser_with(&settings, &env, None);
        assert!(!without.state.is_ready(), "got {:?}", without.state);

        let with = check_browser_with(&settings, &env, Some("http://127.0.0.1:9333"));
        assert!(with.state.is_ready(), "got {:?}", with.state);
        // The provider still runs as a process, so it still needs its runtime:
        // answering Ready without one would hand the session tools that cannot
        // start.
        assert!(with.node_executable.is_some());
    }

    #[test]
    fn a_panel_browser_does_not_excuse_a_missing_provider() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };

        let preflight = check_browser_with(
            &settings,
            &FakeEnv::new().with_node().without_provider(),
            Some("http://127.0.0.1:9333"),
        );

        assert!(!preflight.state.is_ready(), "got {:?}", preflight.state);
    }

    #[test]
    fn a_panel_browser_does_not_excuse_a_missing_node() {
        // Without a Node runtime the provider cannot start at all, whatever
        // browser the tools would have driven.
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };

        let preflight =
            check_browser_with(&settings, &FakeEnv::new(), Some("http://127.0.0.1:9333"));

        assert!(!preflight.state.is_ready(), "got {:?}", preflight.state);
    }

    #[test]
    fn a_panel_browser_makes_a_stale_executable_path_irrelevant() {
        // Nothing is launched while the panel is the browser, so a path that
        // points nowhere must not withhold the tools.
        let settings = BrowserSettings {
            enabled: true,
            executable_path: "/nope/chromium".to_string(),
            ..BrowserSettings::default()
        };

        let preflight = check_browser_with(
            &settings,
            &FakeEnv::new().with_node(),
            Some("http://127.0.0.1:9333"),
        );

        assert!(preflight.state.is_ready(), "got {:?}", preflight.state);
    }

    #[test]
    fn enabled_browser_with_ready_preflight_injects() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };
        let preflight = BrowserPreflight {
            state: PreflightState::Ready,
            node_executable: Some(PathBuf::from("/usr/bin/node")),
            provider_version: "0.1.6-alpha.1".to_string(),
        };

        assert_eq!(
            browser_injection_decision(&settings, &preflight),
            BrowserInjectionDecision::Inject
        );
    }

    #[test]
    fn enabled_browser_with_failed_preflight_withholds_tools_with_a_reason() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };
        let preflight = BrowserPreflight {
            state: PreflightState::Missing {
                detail: "no node".to_string(),
                remedy: "install node".to_string(),
                fix: PreflightFix::Install,
            },
            node_executable: None,
            provider_version: "0.1.6-alpha.1".to_string(),
        };

        match browser_injection_decision(&settings, &preflight) {
            BrowserInjectionDecision::Unavailable(PreflightState::Missing { detail, .. }) => {
                assert_eq!(detail, "no node");
            }
            other => panic!("expected Unavailable(Missing), got {other:?}"),
        }
    }

    /// The serialized shape the settings pane's install button reads.
    ///
    /// Asserting the Rust enum is not enough: a `rename_all` or `tag` change
    /// would keep every struct-level test green while the pane's
    /// `state.fix === "install"` quietly stopped matching. This asserts the
    /// bytes on the wire, using the exact key names the TypeScript type uses.
    #[test]
    fn an_unprovisioned_host_serializes_the_fix_the_button_reads() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };
        let preflight = check_browser(&settings, &FakeEnv::new().with_node().without_provider());

        let json = serde_json::to_value(&preflight).unwrap();
        assert_eq!(json["state"]["state"], "missing");
        assert_eq!(
            json["state"]["fix"], "install",
            "the pane checks state.fix === \"install\"; got {json}"
        );
    }

    #[test]
    fn a_node_less_host_serializes_configure_so_no_button_is_offered() {
        let settings = BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        };
        let preflight = check_browser(&settings, &FakeEnv::new());

        let json = serde_json::to_value(&preflight).unwrap();
        assert_eq!(json["state"]["fix"], "configure");
    }

    #[test]
    fn computer_use_defaults_to_disabled_and_validates() {
        let settings = ComputerUseSettings::default();
        assert!(!settings.enabled, "computer-use must default to disabled");
        assert!(settings.validate().is_ok());

        let preflight = check_computer_use(&settings);
        if platform_supported() {
            assert!(preflight.state.is_ready());
            // Disabled settings withhold the tools even though preflight passes.
            assert_eq!(
                computer_injection_decision(&settings, &preflight),
                ComputerInjectionDecision::Disabled
            );
        }
    }

    #[test]
    fn enabled_computer_use_with_ready_preflight_injects() {
        let settings = ComputerUseSettings {
            enabled: true,
            ..ComputerUseSettings::default()
        };
        let preflight = check_computer_use(&settings);

        if platform_supported() {
            assert_eq!(
                computer_injection_decision(&settings, &preflight),
                ComputerInjectionDecision::Inject
            );
        }
    }

    #[test]
    fn zero_timeout_is_rejected_for_both_capabilities() {
        let browser = BrowserSettings {
            tool_call_timeout_ms: 0,
            ..BrowserSettings::default()
        };
        assert!(browser.validate().is_err());

        let computer = ComputerUseSettings {
            tool_call_timeout_ms: 0,
            ..ComputerUseSettings::default()
        };
        assert!(computer.validate().is_err());
    }

    #[test]
    fn settings_round_trip_through_serde_with_defaults() {
        // An existing settings file has no browser/computer keys; loading it
        // must produce disabled capabilities rather than failing.
        let raw = r#"{"selectedAgent":"codex-acp","theme":"graphite"}"#;
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(parsed.get("browser").is_none());
        assert!(parsed.get("computerUse").is_none());
    }
}
