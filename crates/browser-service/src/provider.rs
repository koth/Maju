//! Provider process construction.
//!
//! The browser provider is a pinned `@playwright/mcp` package launched under
//! the current Node executable. This module owns exactly one decision — the
//! argv and environment for a given configuration — kept separate from
//! process management so the argument rules can be tested without ever
//! spawning a browser.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use workspace_model::{BrowserMode, BrowserSettings};

/// A fully resolved provider launch description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLaunch {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

impl ProviderLaunch {
    /// Environment for the child: the parent's environment minus any
    /// `PLAYWRIGHT_MCP_*` overrides, plus the host's system proxy.
    ///
    /// DSH clears the overrides because the provider reads them as
    /// configuration. Letting a stray value from the developer's shell silently
    /// change the browser the user gets is exactly the kind of surprise this
    /// capability should not have.
    ///
    /// The proxy is added because the provider is a Node process and Node reads
    /// proxy configuration from the environment only. Chromium, which does the
    /// browsing, reads the system proxy itself — so this concerns the provider's
    /// own network calls, not the pages it loads.
    pub fn child_env(parent: &HashMap<String, String>) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = parent
            .iter()
            .filter(|(key, _)| !key.to_ascii_uppercase().starts_with("PLAYWRIGHT_MCP_"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        crate::proxy::apply_to(&mut env);
        env
    }

    /// Put the directory holding `executable` on the child's `PATH`.
    ///
    /// The provider itself is started as `<node> <cli.js>`, so its interpreter
    /// is found by absolute path and does not need this. Anything the provider
    /// then launches by name does: Playwright resolves helpers and its browser
    /// driver through `PATH`. A packaged app's `PATH` is
    /// `/usr/bin:/bin:/usr/sbin:/sbin`, with no Homebrew and no version
    /// manager, so on the user's own machine the browser would fail to start
    /// for the same reason the install did.
    ///
    /// A bare name yields no directory and is left alone, since prepending
    /// nothing is the only honest option when there is no path to add.
    pub fn prepend_executable_dir(env: &mut HashMap<String, String>, executable: &Path) {
        const PATH_KEY: &str = "PATH";

        let Some(dir) = executable.parent().filter(|d| !d.as_os_str().is_empty()) else {
            return;
        };
        let dir = dir.to_string_lossy().into_owned();

        let existing = std::env::split_paths(&env.get(PATH_KEY).cloned().unwrap_or_default())
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|entry| !entry.is_empty() && entry != &dir)
            .collect::<Vec<_>>();

        let mut entries = Vec::with_capacity(existing.len() + 1);
        entries.push(dir);
        entries.extend(existing);
        if let Ok(joined) = std::env::join_paths(entries.iter().map(PathBuf::from)) {
            env.insert(PATH_KEY.to_string(), joined.to_string_lossy().into_owned());
        }
    }
}

/// The package specifier for the pinned provider.
pub fn provider_package(pinned_version: &str) -> String {
    format!("@playwright/mcp@{pinned_version}")
}

/// Resolve the provider entry point inside an installed package tree.
///
/// Given the directory that contains the provider's `package.json`, return the
/// CLI path. Kept as a pure function so the layout rule is testable.
pub fn provider_cli_from_package_root(package_root: &std::path::Path) -> PathBuf {
    package_root.join("cli.js")
}

/// Add the headless flag, or leave it out.
///
/// `--headless` is a valueless switch and the provider is **headed** by
/// default, so there is nothing to pass for a headed browser. Passing
/// `--headless=false` is not a synonym for omitting it: the provider rejects the
/// argument as unknown, exits, and the caller sees a closed connection rather
/// than a window.
fn push_headless(args: &mut Vec<String>, headless: bool) {
    if headless {
        args.push("--headless".to_string());
    }
}

/// Build the launch description for a configuration.
///
/// `resolve_package_root` locates the installed provider; returning `None`
/// means preflight should have failed, and the caller surfaces that rather than
/// launching a process that cannot work.
pub fn build_launch(
    settings: &BrowserSettings,
    node_executable: PathBuf,
    package_root: std::path::PathBuf,
    profile_dir: PathBuf,
) -> ProviderLaunch {
    let mut args = vec![
        provider_cli_from_package_root(&package_root)
            .to_string_lossy()
            .into_owned(),
    ];

    match settings.mode {
        BrowserMode::Launch => {
            // `--isolated` keeps the session's browser free of any user
            // profile, so two sessions cannot share cookies or cache.
            args.push("--isolated".to_string());
            args.push("--browser".to_string());
            args.push("chromium".to_string());
            push_headless(&mut args, settings.headless);
            let executable = settings.executable_path.trim();
            if !executable.is_empty() {
                args.push("--executable-path".to_string());
                args.push(executable.to_string());
            }
        }
        BrowserMode::Attach => {
            // Attaching means the browser is the user's own process, so no
            // `--isolated` and no headless flag: the user can see it.
            args.push("--cdp-endpoint".to_string());
            args.push(settings.endpoint.trim().to_string());
        }
        BrowserMode::Persistent => {
            // A Kodex-owned profile directory. No `--isolated`, because the
            // point is to keep cookies between sessions, but never the user's
            // own profile: the path is built under Kodex's data root.
            args.push("--browser".to_string());
            args.push("chromium".to_string());
            push_headless(&mut args, settings.headless);
            args.push("--user-data-dir".to_string());
            args.push(profile_dir.to_string_lossy().into_owned());
        }
    }

    ProviderLaunch {
        executable: node_executable,
        args,
        env: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch_settings() -> BrowserSettings {
        BrowserSettings {
            enabled: true,
            executable_path: String::new(),
            endpoint: String::new(),
            ..BrowserSettings::default()
        }
    }

    fn launch(settings: &BrowserSettings) -> ProviderLaunch {
        build_launch(
            settings,
            PathBuf::from("/usr/bin/node"),
            PathBuf::from("/pkg/node_modules/@playwright/mcp"),
            PathBuf::from("/data/browser/profiles/default"),
        )
    }

    fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .map(String::as_str)
    }

    #[test]
    fn launch_mode_isolates_and_defaults_to_headless_chromium() {
        let args = launch(&launch_settings()).args;

        assert!(args[0].ends_with("cli.js"), "got {}", args[0]);
        assert!(args.contains(&"--isolated".to_string()));
        assert_eq!(arg_after(&args, "--browser"), Some("chromium"));
        // Valueless switch, and the provider is headed by default — this exact
        // spelling is what the real provider accepts. `--headless=true` is not
        // a synonym; the provider exits on it.
        assert!(args.contains(&"--headless".to_string()), "got {args:?}");
        assert!(
            !args.iter().any(|arg| arg.starts_with("--headless=")),
            "the flag takes no value: {args:?}"
        );
    }

    #[test]
    fn launch_mode_honours_headless_false_by_omitting_the_flag() {
        let settings = BrowserSettings {
            headless: false,
            ..launch_settings()
        };
        let args = launch(&settings).args;
        // Headed is the provider's default, so the way to ask for it is to say
        // nothing. Passing `--headless=false` gets the process killed.
        assert!(
            !args.iter().any(|arg| arg.starts_with("--headless")),
            "a headed browser is the absence of the flag: {args:?}"
        );
    }

    #[test]
    fn launch_mode_passes_an_explicit_executable_only_when_set() {
        let mut args = launch(&launch_settings()).args;
        assert!(!args.iter().any(|arg| arg == "--executable-path"));

        let settings = BrowserSettings {
            executable_path: "/opt/chrome".to_string(),
            ..launch_settings()
        };
        args = launch(&settings).args;
        assert_eq!(arg_after(&args, "--executable-path"), Some("/opt/chrome"));
    }

    #[test]
    fn launch_mode_ignores_a_blank_executable_path() {
        let settings = BrowserSettings {
            executable_path: "   ".to_string(),
            ..launch_settings()
        };
        let args = launch(&settings).args;
        assert!(!args.iter().any(|arg| arg == "--executable-path"));
    }

    #[test]
    fn attach_mode_targets_the_endpoint_and_never_isolates() {
        let settings = BrowserSettings {
            mode: BrowserMode::Attach,
            endpoint: "http://127.0.0.1:9222".to_string(),
            allow_attach: true,
            ..launch_settings()
        };
        let args = launch(&settings).args;

        assert_eq!(
            arg_after(&args, "--cdp-endpoint"),
            Some("http://127.0.0.1:9222")
        );
        // Attaching to a browser the user can see means no headless override.
        assert!(!args.iter().any(|arg| arg.starts_with("--headless")));
        assert!(!args.contains(&"--isolated".to_string()));
    }

    #[test]
    fn persistent_mode_uses_a_profile_dir_and_is_not_isolated() {
        let settings = BrowserSettings {
            mode: BrowserMode::Persistent,
            profile_name: "default".to_string(),
            ..launch_settings()
        };
        let args = launch(&settings).args;

        assert_eq!(
            arg_after(&args, "--user-data-dir"),
            Some("/data/browser/profiles/default")
        );
        // Keeping cookies between sessions is the entire point, so the
        // provider must not be told to isolate.
        assert!(!args.contains(&"--isolated".to_string()));
        // It is still an owned browser, so it can be headless.
        assert!(args.contains(&"--headless".to_string()), "got {args:?}");
    }

    #[test]
    fn persistent_mode_never_points_at_a_real_browser_profile() {
        // The one thing this mode must never do is hand Chromium a path
        // outside Kodex's data root.
        let settings = BrowserSettings {
            mode: BrowserMode::Persistent,
            // A name that tries to escape the profiles directory.
            profile_name: "../../../Library/Application Support/Google/Chrome".to_string(),
            ..launch_settings()
        };
        let args = launch(&settings).args;
        let dir = arg_after(&args, "--user-data-dir").unwrap_or_default();
        assert!(
            dir.starts_with("/data/browser/profiles/"),
            "profile directory escaped the data root: {dir}",
        );
    }

    #[test]
    fn persistent_mode_takes_the_profile_from_the_launch_path() {
        // `build_launch` is handed the resolved directory, so the profile name
        // is resolved once, by the caller, against validated settings.
        let launch = build_launch(
            &BrowserSettings {
                mode: BrowserMode::Persistent,
                profile_name: "staging".to_string(),
                ..launch_settings()
            },
            PathBuf::from("/usr/bin/node"),
            PathBuf::from("/pkg/@playwright/mcp"),
            PathBuf::from("/data/browser/profiles/staging"),
        );
        assert_eq!(
            arg_after(&launch.args, "--user-data-dir"),
            Some("/data/browser/profiles/staging"),
        );
    }

    #[test]
    fn attach_mode_trims_the_endpoint() {
        let settings = BrowserSettings {
            mode: BrowserMode::Attach,
            endpoint: "  ws://127.0.0.1:9222/devtools  ".to_string(),
            allow_attach: true,
            ..launch_settings()
        };
        assert_eq!(
            arg_after(&launch(&settings).args, "--cdp-endpoint"),
            Some("ws://127.0.0.1:9222/devtools"),
        );
    }

    #[test]
    fn child_env_strips_every_case_of_the_provider_prefix() {
        let parent: HashMap<String, String> = [
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("PLAYWRIGHT_MCP_HEADLESS".to_string(), "false".to_string()),
            ("playwright_mcp_browser".to_string(), "firefox".to_string()),
            ("Playwright_Mcp_Executable".to_string(), "/x".to_string()),
            ("MY_PLAYWRIGHT_MCP_THING".to_string(), "keep".to_string()),
        ]
        .into_iter()
        .collect();

        let child = ProviderLaunch::child_env(&parent);

        assert_eq!(child.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert!(!child.contains_key("PLAYWRIGHT_MCP_HEADLESS"));
        assert!(!child.contains_key("playwright_mcp_browser"));
        assert!(!child.contains_key("Playwright_Mcp_Executable"));
        // The prefix must match at the start of the name, not anywhere in it.
        assert_eq!(
            child.get("MY_PLAYWRIGHT_MCP_THING").map(String::as_str),
            Some("keep")
        );
    }

    #[test]
    fn the_executable_directory_is_put_on_the_childs_path() {
        // The case that matters: a GUI-launched app's PATH, with the tool
        // installed somewhere it does not mention.
        let mut env: HashMap<String, String> = [("PATH".to_string(), "/usr/bin:/bin".to_string())]
            .into_iter()
            .collect();

        ProviderLaunch::prepend_executable_dir(&mut env, Path::new("/opt/homebrew/bin/node"));

        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some("/opt/homebrew/bin:/usr/bin:/bin"),
        );
    }

    #[test]
    fn a_bare_name_adds_nothing_rather_than_a_bogus_entry() {
        // Preflight's fallback is the bare name `node`; prepending an empty or
        // `.` entry would be worse than leaving the PATH alone.
        let mut env: HashMap<String, String> = [("PATH".to_string(), "/usr/bin".to_string())]
            .into_iter()
            .collect();

        ProviderLaunch::prepend_executable_dir(&mut env, Path::new("node"));

        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
    }

    #[test]
    fn an_existing_entry_is_not_duplicated() {
        let mut env: HashMap<String, String> =
            [("PATH".to_string(), "/opt/homebrew/bin:/usr/bin".to_string())]
                .into_iter()
                .collect();

        ProviderLaunch::prepend_executable_dir(&mut env, Path::new("/opt/homebrew/bin/node"));

        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some("/opt/homebrew/bin:/usr/bin"),
        );
    }

    #[test]
    fn an_executable_with_no_directory_leaves_a_missing_path_alone() {
        // There is nothing to add, and inventing an entry would put the
        // provider's working directory on its PATH.
        let mut env = HashMap::new();
        ProviderLaunch::prepend_executable_dir(&mut env, Path::new("node"));
        assert!(env.get("PATH").is_none());
    }

    #[test]
    fn provider_specifier_pins_the_configured_version() {
        assert_eq!(
            provider_package("0.1.6-alpha.1"),
            "@playwright/mcp@0.1.6-alpha.1"
        );
    }
}
