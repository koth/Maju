//! The browser Kodex starts itself, and how it is found.
//!
//! Tool-driven browsing and the right-panel view are two clients of one
//! browser, so in launch and persistent mode Kodex owns that browser: it
//! starts Chromium with a debugging port, hands the provider the resulting
//! `--cdp-endpoint`, and the view connects to the same websocket URL. This
//! module owns the two facts the rest of the subsystem depends on — which
//! executable to start, and what a started browser reports about itself.
//!
//! Both facts come from the outside world, so both are seams. Executable
//! resolution runs through [`CommandRunner`] and launching through
//! [`BrowserHost`], which keeps the argument assembly, the profile rules, and
//! the readiness logic testable without a browser or an npm tree on the
//! machine.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use workspace_model::{BrowserMode, BrowserSettings};

use crate::installer::HostCommandRunner;
use crate::provision::{CommandOutcome, CommandRunner};

/// How long the one-shot executable resolution may run.
///
/// It starts Node and requires one package; anything longer than this is a
/// machine in trouble, and the user is better served by the reason than by a
/// session that waits forever for an answer that is not coming.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a started browser gets to expose its debugging port.
///
/// Chromium reaches `/json/version` in a second or two even on a cold start;
/// fifteen seconds is the allowance for a slow disk, not a licence to wait.
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How often readiness is checked while the browser starts.
pub const PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// The one-shot script that prints the Chromium executable Playwright knows
/// about.
///
/// The package root is spelled out in absolute `require` paths *and* pushed
/// onto the module search path, because where `playwright-core` physically
/// lives depends on how npm laid the tree out: inside the package's own
/// `node_modules`, or hoisted next to it. The bare names cover both through
/// the search path, and `playwright` is the documented last resort for trees
/// that carry the full package rather than its core.
///
/// Kept as a pure function so the request is testable without Node.
///
/// The script travels as a single `node -e` argument, so it is deliberately
/// **one line**: `node` on PATH is often a shim (Volta's `node.exe` re-launches
/// the real binary), and a shim that rebuilds the command line truncates an
/// argument at its first newline. A multi-line script then runs only its first
/// statement — `const root = …;` — and exits 0 with empty output, which reads as
/// "Node printed no path" and takes the whole browser provider down with it.
pub fn resolve_script(package_root: &Path) -> String {
    let node_modules = package_root.join("node_modules");
    // npm hoists a dependency of `<prefix>/node_modules/<pkg>` to
    // `<prefix>/node_modules/<dep>`, two directory names above the package
    // root. The `..` components are left in place: Node resolves them away,
    // and spelling the paths out keeps this function honest about both
    // layouts.
    let hoisted = package_root.join("..").join("..");

    let search_dirs = [
        node_modules.to_string_lossy().into_owned(),
        hoisted.to_string_lossy().into_owned(),
    ];
    let candidates = [
        node_modules.join("playwright-core").to_string_lossy().into_owned(),
        hoisted.join("playwright-core").to_string_lossy().into_owned(),
        "playwright-core".to_string(),
        "playwright".to_string(),
    ];

    let root = package_root.to_string_lossy().into_owned();
    [
        format!("const root = {};", json_string(&root)),
        format!("const dirs = {};", json_string_list(&search_dirs)),
        "for (const dir of dirs) { try { module.paths.unshift(dir); } catch (e) {} }".to_string(),
        format!("const candidates = {};", json_string_list(&candidates)),
        "let found = \"\";".to_string(),
        "for (const name of candidates) { try { const mod = require(name); \
         const chromium = mod && (mod.chromium || (mod.default && mod.default.chromium)); \
         const exe = chromium && chromium.executablePath(); \
         if (exe) { found = String(exe); break; } } catch (e) {} }"
            .to_string(),
        "if (found) { process.stdout.write(found); } \
         else { process.stderr.write(\"playwright-core not found under \" + root); \
         process.exitCode = 1; }"
            .to_string(),
    ]
    .join(" ")
}

/// A JS string literal for `value`.
fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("a string is always serializable")
}

/// A JS array literal of string literals for `values`.
fn json_string_list(values: &[String]) -> String {
    serde_json::to_string(values).expect("a list of strings is always serializable")
}

/// Resolve the Chromium executable to start.
///
/// `settings.executable_path` wins when it is set: the user named a browser
/// and discovery must not second-guess it. Otherwise the pinned package's
/// Playwright is asked which Chromium it manages, through a one-shot Node
/// script — asking the same tree that will run the provider is what keeps the
/// answer on the revision the provider itself would use.
pub async fn resolve_browser_executable(
    node: &Path,
    package_root: &Path,
    settings: &BrowserSettings,
) -> Result<PathBuf, String> {
    resolve_browser_executable_with(&HostCommandRunner, node, package_root, settings).await
}

/// [`resolve_browser_executable`] through an injected runner.
///
/// The runner is the only outside world this needs, so the request and the
/// outcome handling are testable without Node, npm, or a browser.
pub async fn resolve_browser_executable_with(
    runner: &dyn CommandRunner,
    node: &Path,
    package_root: &Path,
    settings: &BrowserSettings,
) -> Result<PathBuf, String> {
    let configured = settings.executable_path.trim();
    if !configured.is_empty() {
        return Ok(PathBuf::from(configured));
    }

    let args = vec!["-e".to_string(), resolve_script(package_root)];
    // The parent's environment, unfiltered: the answer depends on where the
    // browsers were installed (`PLAYWRIGHT_BROWSERS_PATH` and friends), and
    // guessing around the user's own environment would resolve a browser
    // other than the one the install created.
    let parent: std::collections::HashMap<String, String> = std::env::vars().collect();
    let outcome = runner
        .run(&node.to_string_lossy(), &args, &parent, RESOLVE_TIMEOUT)
        .await;

    if let Some(error) = &outcome.spawn_error {
        return Err(format!("could not resolve the browser executable: {error}"));
    }
    if outcome.timed_out {
        return Err(format!(
            "resolving the browser executable took longer than {RESOLVE_TIMEOUT:?}"
        ));
    }
    if !outcome.success {
        return Err(format!(
            "could not resolve the browser executable: {}",
            command_detail(&outcome)
        ));
    }

    let resolved = outcome.stdout.trim();
    if resolved.is_empty() {
        return Err("the browser executable script printed no path".to_string());
    }
    Ok(PathBuf::from(resolved))
}

/// The most useful line of a failed one-shot script.
fn command_detail(outcome: &CommandOutcome) -> String {
    for stream in [&outcome.stderr, &outcome.stdout] {
        let line = stream.trim();
        if !line.is_empty() {
            return line.to_string();
        }
    }
    match outcome.status {
        Some(status) => format!("the script exited with status {status}"),
        None => "the script produced no output".to_string(),
    }
}

/// A browser process [`ManagedBrowser::shutdown`] can kill.
#[async_trait::async_trait]
pub trait BrowserChild: Send + Sync {
    /// Terminate the browser and reap it.
    async fn kill(&mut self) -> Result<(), String>;
}

/// Everything a launch needs from the outside world: starting the process and
/// observing its debugging endpoint.
///
/// Injected so the launch sequence — argument assembly, profile rules,
/// readiness polling — is testable without starting a Chromium.
#[async_trait::async_trait]
pub trait BrowserHost: Send + Sync {
    /// Start `executable` with `args`; what comes back is killed on shutdown.
    async fn spawn(
        &self,
        executable: &Path,
        args: &[String],
    ) -> Result<Box<dyn BrowserChild>, String>;

    /// One readiness observation: the `webSocketDebuggerUrl` reported by
    /// `http://127.0.0.1:<port>/json/version`, or `None` while the browser is
    /// still starting.
    async fn probe_ws_endpoint(&self, port: u16) -> Option<String>;
}

/// A Chromium that Kodex started, with the debugging port the provider
/// attaches to.
///
/// This is the browser behind `--cdp-endpoint` when the panel's own browser is
/// not the one being driven — the fallback the settings ask for. When the panel
/// browser is up, `BrowserFactory::acquire` returns its endpoint instead and no
/// managed Chromium is launched at all.
pub struct ManagedBrowser {
    child: Option<Box<dyn BrowserChild>>,
    port: u16,
    ws_endpoint: String,
    /// A temporary profile, deleted on shutdown. `None` in persistent mode,
    /// where the profile outlives the session on purpose.
    temp_profile: Option<PathBuf>,
}

impl ManagedBrowser {
    /// Start a managed Chromium for one session.
    ///
    /// Launch mode gets a fresh profile under the data root's temporary area;
    /// persistent mode gets its named profile, which is kept. Neither is ever
    /// the user's own browser profile.
    pub async fn launch(
        data_root: &Path,
        executable: &Path,
        settings: &BrowserSettings,
    ) -> Result<ManagedBrowser, String> {
        Self::launch_with(&HostBrowser, data_root, executable, settings).await
    }

    /// [`ManagedBrowser::launch`] through an injected host.
    pub async fn launch_with(
        host: &dyn BrowserHost,
        data_root: &Path,
        executable: &Path,
        settings: &BrowserSettings,
    ) -> Result<ManagedBrowser, String> {
        let (profile, temporary) = profile_for(data_root, settings)?;
        // Chromium refuses to start against a missing `--user-data-dir`, so
        // the directory is created before the process — the failure surfaces
        // here, next to the setting that chose it, instead of as a browser
        // that exits on its first run.
        std::fs::create_dir_all(&profile)
            .map_err(|error| format!("could not create {}: {error}", profile.display()))?;

        let port = free_port()?;
        let args = browser_argv(port, &profile, settings.headless);
        let mut child = host.spawn(executable, &args).await?;

        let started = tokio::time::Instant::now();
        let ws_endpoint = loop {
            if let Some(ws_endpoint) = host.probe_ws_endpoint(port).await {
                break ws_endpoint;
            }
            if started.elapsed() >= READY_TIMEOUT {
                // The process is ours; a browser that never came up must not
                // outlive the failure.
                let _ = child.kill().await;
                if temporary {
                    let _ = std::fs::remove_dir_all(&profile);
                }
                return Err(format!(
                    "the browser did not expose a CDP endpoint on 127.0.0.1:{port} \
                     within {READY_TIMEOUT:?}"
                ));
            }
            tokio::time::sleep(PROBE_INTERVAL).await;
        };

        Ok(ManagedBrowser {
            child: Some(child),
            port,
            ws_endpoint,
            temp_profile: temporary.then_some(profile),
        })
    }

    /// The endpoint both clients attach to: `webSocketDebuggerUrl` from the
    /// browser's `/json/version`.
    pub fn ws_endpoint(&self) -> &str {
        &self.ws_endpoint
    }

    /// The debugging port the browser was started with.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Kill the browser and clean up a temporary profile.
    ///
    /// A persistent profile is left alone: keeping it between sessions is the
    /// entire point of persistent mode.
    pub async fn shutdown(mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        if let Some(profile) = self.temp_profile.take() {
            let _ = std::fs::remove_dir_all(profile);
        }
    }
}

impl Drop for ManagedBrowser {
    /// A temporary profile must not outlive its browser — not even a launch
    /// that failed after the spawn, where nothing calls [`Self::shutdown`].
    /// The process itself is the child's `kill_on_drop` to reap.
    fn drop(&mut self) {
        if let Some(profile) = self.temp_profile.take() {
            let _ = std::fs::remove_dir_all(profile);
        }
    }
}

/// argv for a managed Chromium, given the debugging port and profile
/// directory.
///
/// `about:blank` is the starting page: the session's browsing happens on
/// targets the tool and the view open, and a real start page would be a
/// request nobody asked for. `--headless=new` is the current headless mode —
/// same rendering as a window, without one — and is omitted for a headed
/// browser the user can see.
pub fn browser_argv(port: u16, profile: &Path, headless: bool) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        format!("--user-data-dir={}", profile.to_string_lossy()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ];
    if headless {
        args.push("--headless=new".to_string());
    }
    args.push("about:blank".to_string());
    args
}

/// The profile directory for a managed browser, and whether it is temporary.
///
/// Launch mode gets a fresh directory deleted when the browser goes away, so
/// nothing carries over between sessions. Persistent mode gets its named
/// profile under the data root, kept between sessions — never the user's own
/// browser profile, which no mode may read or write.
fn profile_for(data_root: &Path, settings: &BrowserSettings) -> Result<(PathBuf, bool), String> {
    match settings.mode {
        BrowserMode::Launch => Ok((
            data_root.join("tmp").join("browser").join(unique_profile_name()),
            true,
        )),
        BrowserMode::Persistent => Ok((
            crate::provision::profile_dir(data_root, &settings.profile_name),
            false,
        )),
        BrowserMode::Attach => Err("attach mode does not manage a browser".to_string()),
    }
}

/// A unique name for a temporary profile directory.
///
/// Unique on this machine for this process's lifetime, which is all a
/// temporary profile needs: the pid, the clock, and a counter against two
/// launches inside the same nanosecond.
fn unique_profile_name() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}-{sequence}", std::process::id())
}

/// An unused TCP port on the loopback interface.
///
/// The listener is dropped before the browser binds it, so another process
/// can in principle take the port in between. The alternative — passing a
/// bound listener to a process that expects a port number — does not exist,
/// and the collision is both unlikely and reported as a startup failure.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("could not allocate a debugging port: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("could not read the debugging port: {error}"))?
        .port();
    Ok(port)
}

/// A [`BrowserHost`] backed by the real machine.
pub struct HostBrowser;

impl HostBrowser {
    /// One readiness observation: `GET /json/version`, parsed for the
    /// websocket URL.
    ///
    /// A hand-rolled HTTP exchange because this is one fixed request against
    /// a loopback socket, and an HTTP client dependency is a large price for
    /// a single `GET`. Runs on a blocking thread so the wait does not hold up
    /// the runtime.
    fn probe_once(port: u16) -> Option<String> {
        use std::io::{Read, Write};

        let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let mut stream =
            std::net::TcpStream::connect_timeout(&address, Duration::from_millis(500)).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .ok()?;
        stream
            .set_write_timeout(Some(Duration::from_millis(500)))
            .ok()?;
        write!(
            stream,
            "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        )
        .ok()?;

        // Read until the answer carries the websocket URL, then stop.
        //
        // Waiting for the stream to end instead would never succeed: Chromium's
        // DevTools server answers and then *keeps the connection open*, `Connection:
        // close` notwithstanding, so the read that follows a complete answer only
        // returns on its timeout — and a probe that treats that as a failure throws
        // away a browser that is up and listening on the very port it asked about.
        let mut response = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    response.extend_from_slice(&chunk[..read]);
                    if let Some(endpoint) = websocket_url(&String::from_utf8_lossy(&response)) {
                        return Some(endpoint);
                    }
                }
                // A timeout means "nothing more for now"; what already arrived
                // is still worth parsing.
                Err(_) => break,
            }
        }
        websocket_url(&String::from_utf8_lossy(&response))
    }
}

/// The `webSocketDebuggerUrl` in a `/json/version` answer, once enough of the
/// answer has arrived to carry it.
///
/// `None` while the answer is still incomplete, which is what lets the probe
/// stop reading as soon as the field shows up rather than waiting for a close.
fn websocket_url(response: &str) -> Option<String> {
    if !response.starts_with("HTTP/1.1 200") {
        return None;
    }
    // The body is parsed from its first `{` onwards rather than trusted
    // to be exactly one JSON document: chunked framing and a trailing
    // newline would otherwise turn a correct answer into a failed probe.
    let body = &response[response.find('{')?..];
    let value = serde_json::Deserializer::from_str(body)
        .into_iter::<serde_json::Value>()
        .next()?
        .ok()?;
    value
        .get("webSocketDebuggerUrl")?
        .as_str()
        .map(str::to_string)
}

#[async_trait::async_trait]
impl BrowserHost for HostBrowser {
    async fn spawn(
        &self,
        executable: &Path,
        args: &[String],
    ) -> Result<Box<dyn BrowserChild>, String> {
        use std::process::Stdio;

        let mut command = tokio::process::Command::new(executable);
        command
            .args(args)
            // The browser is driven over its debugging port and has no use
            // for Kodex's stdio.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Reap it even if shutdown is never reached, e.g. on panic.
            .kill_on_drop(true);

        // No console window: Chromium is a console-subsystem program started
        // from a GUI app. See `crate::win`.
        crate::win::hide_console(&mut command);

        let child = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", executable.display()))?;
        Ok(Box::new(HostBrowserChild(child)))
    }

    async fn probe_ws_endpoint(&self, port: u16) -> Option<String> {
        tokio::task::spawn_blocking(move || Self::probe_once(port))
            .await
            .ok()
            .flatten()
    }
}

struct HostBrowserChild(tokio::process::Child);

#[async_trait::async_trait]
impl BrowserChild for HostBrowserChild {
    async fn kill(&mut self) -> Result<(), String> {
        self.0
            .start_kill()
            .map_err(|error| format!("could not kill the browser: {error}"))?;
        // Reap, so the process is not left behind as a zombie until drop.
        let _ = self.0.wait().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    fn ok_outcome(stdout: &str) -> CommandOutcome {
        CommandOutcome {
            success: true,
            status: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
            spawn_error: None,
            timed_out: false,
        }
    }

    fn fail_outcome(stderr: &str) -> CommandOutcome {
        CommandOutcome {
            success: false,
            status: Some(1),
            stdout: String::new(),
            stderr: stderr.to_string(),
            spawn_error: None,
            timed_out: false,
        }
    }

    /// A runner that records its calls and answers with a scripted outcome,
    /// in the shape of `installer`'s scripted runner.
    struct ScriptedRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
        outcome: CommandOutcome,
    }

    impl ScriptedRunner {
        fn new(outcome: CommandOutcome) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outcome,
            }
        }

        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl CommandRunner for ScriptedRunner {
        async fn run(
            &self,
            program: &str,
            args: &[String],
            _env: &std::collections::HashMap<String, String>,
            _timeout: Duration,
        ) -> CommandOutcome {
            self.calls
                .lock()
                .unwrap()
                .push((program.to_string(), args.to_vec()));
            self.outcome.clone()
        }
    }

    /// A host that records the launch and scripts the readiness answers.
    struct ScriptedHost {
        spawns: Mutex<Vec<(PathBuf, Vec<String>)>>,
        probes: Mutex<VecDeque<Option<String>>>,
        killed: Arc<AtomicBool>,
    }

    impl ScriptedHost {
        fn new(probes: Vec<Option<String>>) -> Self {
            Self {
                spawns: Mutex::new(Vec::new()),
                probes: Mutex::new(probes.into()),
                killed: Arc::new(AtomicBool::new(false)),
            }
        }

        /// Answers the first probe, like a browser that starts promptly.
        fn ready(ws_endpoint: &str) -> Self {
            Self::new(vec![Some(ws_endpoint.to_string())])
        }

        fn spawned(&self) -> (PathBuf, Vec<String>) {
            self.spawns.lock().unwrap()[0].clone()
        }

        fn spawned_is_empty(&self) -> bool {
            self.spawns.lock().unwrap().is_empty()
        }

        fn killed(&self) -> bool {
            self.killed.load(Ordering::SeqCst)
        }
    }

    struct ScriptedChild {
        killed: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl BrowserChild for ScriptedChild {
        async fn kill(&mut self) -> Result<(), String> {
            self.killed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl BrowserHost for ScriptedHost {
        async fn spawn(
            &self,
            executable: &Path,
            args: &[String],
        ) -> Result<Box<dyn BrowserChild>, String> {
            self.spawns
                .lock()
                .unwrap()
                .push((executable.to_path_buf(), args.to_vec()));
            Ok(Box::new(ScriptedChild {
                killed: self.killed.clone(),
            }))
        }

        async fn probe_ws_endpoint(&self, _port: u16) -> Option<String> {
            self.probes.lock().unwrap().pop_front().flatten()
        }
    }

    /// A scratch data root, removed by the tests that create browser
    /// profiles inside it.
    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!("kodex-managed-test-{}", unique_profile_name()))
    }

    fn launch_settings() -> BrowserSettings {
        BrowserSettings {
            enabled: true,
            headless: true,
            ..BrowserSettings::default()
        }
    }

    fn user_data_dir(args: &[String]) -> PathBuf {
        let arg = args
            .iter()
            .find(|arg| arg.starts_with("--user-data-dir="))
            .unwrap_or_else(|| panic!("no --user-data-dir in {args:?}"));
        PathBuf::from(arg.trim_start_matches("--user-data-dir="))
    }

    #[tokio::test]
    async fn a_configured_executable_short_circuits_resolution() {
        // The user named a browser; discovery must not second-guess it, and
        // must not start Node to do it.
        let runner = ScriptedRunner::new(ok_outcome("/ignored"));
        let settings = BrowserSettings {
            executable_path: "  /opt/chrome ".to_string(),
            ..launch_settings()
        };

        let resolved =
            resolve_browser_executable_with(&runner, Path::new("node"), Path::new("/pkg"), &settings)
                .await
                .unwrap();

        assert_eq!(resolved, PathBuf::from("/opt/chrome"));
        assert!(runner.calls().is_empty(), "no script should have run");
    }

    #[tokio::test]
    async fn a_blank_executable_path_falls_through_to_discovery() {
        let runner = ScriptedRunner::new(ok_outcome("/apps/chrome\n"));

        let resolved = resolve_browser_executable_with(
            &runner,
            Path::new("/usr/bin/node"),
            Path::new("/pkg/node_modules/@playwright/mcp"),
            &launch_settings(),
        )
        .await
        .unwrap();

        assert_eq!(resolved, PathBuf::from("/apps/chrome"));
    }

    #[test]
    fn the_resolution_script_names_the_package_tree_and_the_fallback() {
        // The argv unit test: what Node is asked, exactly. The package's own
        // node_modules and the hoisted tree are both named, because either
        // can hold playwright-core, and `playwright` is the documented last
        // resort.
        let package_root = Path::new("/pkg/node_modules/@playwright/mcp");
        let script = resolve_script(package_root);
        // The paths inside the script are JSON string literals, so a Windows
        // tree carries escaped separators (`\\`); normalize both sides so the
        // assertions describe the layout rather than the host's separator.
        let script = script.replace("\\\\", "/");
        let package = package_root.to_string_lossy().replace('\\', "/");
        let node_modules = format!("{package}/node_modules");

        assert!(script.contains(&format!("{node_modules}/playwright-core")));
        assert!(script.contains(&format!("{package}/../../playwright-core")));
        assert!(script.contains(&node_modules));
        assert!(script.contains("\"playwright-core\""));
        assert!(script.contains("\"playwright\""));
        assert!(script.contains("chromium.executablePath()"));
    }

    #[test]
    fn the_resolution_script_is_a_single_line() {
        // `node` is reached through whatever is on PATH, which on Windows is
        // often a shim that re-launches the real binary and truncates a
        // multi-line argument at its first newline. A multi-line script then
        // executes only its first statement and exits 0 with no output, so the
        // one property this script must never lose is being one line.
        let script = resolve_script(Path::new("/pkg/node_modules/@playwright/mcp"));

        assert!(!script.contains('\n'), "script must not contain a newline");
        assert!(!script.contains('\r'), "script must not contain a carriage return");
        assert!(
            script.contains("const root = \"/pkg/node_modules/@playwright/mcp\"; const dirs"),
            "statements must stay separated: {script}"
        );
        assert!(
            script.ends_with("process.exitCode = 1; }"),
            "the fallback branch must still close the script: {script}"
        );
    }

    #[tokio::test]
    async fn resolution_runs_one_node_script() {
        let runner = ScriptedRunner::new(ok_outcome("/apps/chrome"));

        resolve_browser_executable_with(
            &runner,
            Path::new("/usr/bin/node"),
            Path::new("/pkg/node_modules/@playwright/mcp"),
            &launch_settings(),
        )
        .await
        .unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "/usr/bin/node");
        assert_eq!(calls[0].1.len(), 2);
        assert_eq!(calls[0].1[0], "-e");
        assert!(calls[0].1[1].contains("playwright-core"));
    }

    #[tokio::test]
    async fn a_failed_resolution_names_the_reason() {
        let runner = ScriptedRunner::new(fail_outcome("Error: Cannot find module 'playwright-core'"));

        let failed = resolve_browser_executable_with(
            &runner,
            Path::new("node"),
            Path::new("/pkg"),
            &launch_settings(),
        )
        .await;

        let message = failed.expect_err("a failed script must not resolve");
        assert!(message.contains("Cannot find module"), "got {message}");
    }

    #[tokio::test]
    async fn a_script_that_prints_nothing_is_a_failure() {
        let runner = ScriptedRunner::new(ok_outcome("  \n"));

        let failed = resolve_browser_executable_with(
            &runner,
            Path::new("node"),
            Path::new("/pkg"),
            &launch_settings(),
        )
        .await;

        assert!(
            failed.expect_err("an empty answer is not a path").contains("printed no path"),
        );
    }

    #[tokio::test]
    async fn managed_launch_assembles_the_documented_arguments() {
        let root = test_root();
        let host = ScriptedHost::ready("ws://127.0.0.1:9222/devtools/browser/abc");

        let browser = ManagedBrowser::launch_with(
            &host,
            &root,
            Path::new("/opt/chrome/chrome"),
            &launch_settings(),
        )
        .await
        .unwrap();

        let (executable, args) = host.spawned();
        assert_eq!(executable, PathBuf::from("/opt/chrome/chrome"));
        assert_eq!(args[0], format!("--remote-debugging-port={}", browser.port()));
        assert!(args[1].starts_with("--user-data-dir="), "got {args:?}");
        assert_eq!(args[2], "--no-first-run");
        assert_eq!(args[3], "--no-default-browser-check");
        assert_eq!(args[4], "--headless=new");
        assert_eq!(args[5], "about:blank");
        assert_eq!(browser.ws_endpoint(), "ws://127.0.0.1:9222/devtools/browser/abc");

        browser.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_headed_browser_gets_no_headless_flag() {
        let root = test_root();
        let host = ScriptedHost::ready("ws://x");
        let settings = BrowserSettings {
            headless: false,
            ..launch_settings()
        };

        let browser =
            ManagedBrowser::launch_with(&host, &root, Path::new("/opt/chrome"), &settings)
                .await
                .unwrap();

        let (_, args) = host.spawned();
        assert!(
            !args.iter().any(|arg| arg.starts_with("--headless")),
            "a headed browser is the absence of the flag: {args:?}"
        );
        browser.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn launch_mode_uses_a_temporary_profile_and_removes_it_on_shutdown() {
        let root = test_root();
        let host = ScriptedHost::ready("ws://x");

        let browser =
            ManagedBrowser::launch_with(&host, &root, Path::new("/opt/chrome"), &launch_settings())
                .await
                .unwrap();

        let (_, args) = host.spawned();
        let profile = user_data_dir(&args);
        assert!(
            profile.starts_with(root.join("tmp").join("browser")),
            "a launch profile must live under the data root: {}",
            profile.display(),
        );
        assert!(profile.is_dir(), "the profile is created before the spawn");

        browser.shutdown().await;
        assert!(
            !profile.exists(),
            "a temporary profile must not survive shutdown"
        );
        assert!(host.killed(), "shutdown must kill the browser");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn persistent_mode_keeps_its_profile() {
        let root = test_root();
        let host = ScriptedHost::ready("ws://x");
        let settings = BrowserSettings {
            mode: BrowserMode::Persistent,
            profile_name: "staging".to_string(),
            ..launch_settings()
        };

        let browser =
            ManagedBrowser::launch_with(&host, &root, Path::new("/opt/chrome"), &settings)
                .await
                .unwrap();

        let (_, args) = host.spawned();
        let profile = user_data_dir(&args);
        assert_eq!(profile, crate::provision::profile_dir(&root, "staging"));
        assert!(profile.is_dir());

        browser.shutdown().await;
        assert!(
            profile.is_dir(),
            "a persistent profile is the point of the mode"
        );
        assert!(host.killed());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn attach_mode_does_not_manage_a_browser() {
        let root = test_root();
        let host = ScriptedHost::ready("ws://x");
        let settings = BrowserSettings {
            mode: BrowserMode::Attach,
            endpoint: "http://127.0.0.1:9222".to_string(),
            allow_attach: true,
            ..launch_settings()
        };

        let failed = ManagedBrowser::launch_with(&host, &root, Path::new("/opt/chrome"), &settings)
            .await;

        let Err(message) = failed else {
            panic!("attach mode has no managed browser");
        };
        assert!(message.contains("attach mode"), "got {message}");
        assert!(host.spawned_is_empty(), "nothing may be spawned");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_probe_reads_an_answer_whose_connection_never_closes() {
        use std::io::Write as _;

        // Chromium's DevTools server answers `/json/version` and then keeps the
        // connection open. A probe that waits for the close never sees the
        // answer, declares a listening browser dead, and kills it.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let holding = Arc::new(AtomicBool::new(true));
        let server_holding = Arc::clone(&holding);
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let body = "{\"Browser\":\"Chrome/154\",\
                        \"webSocketDebuggerUrl\":\"ws://127.0.0.1:9222/devtools/browser/abc\"}";
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.flush();
            // Held open on purpose: no close, exactly like Chromium.
            while server_holding.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let started = std::time::Instant::now();
        let endpoint = HostBrowser::probe_once(port);
        let elapsed = started.elapsed();
        holding.store(false, Ordering::Relaxed);
        server.join().unwrap();

        assert_eq!(
            endpoint.as_deref(),
            Some("ws://127.0.0.1:9222/devtools/browser/abc")
        );
        assert!(
            elapsed < READY_TIMEOUT,
            "the probe waited for a close instead of reading the answer: {elapsed:?}"
        );
    }

    #[test]
    fn the_probe_ignores_an_answer_that_is_not_a_version_document() {
        assert_eq!(websocket_url("HTTP/1.1 404 Not Found\r\n\r\n{}"), None);
        assert_eq!(websocket_url("HTTP/1.1 200 OK\r\n\r\n{\"Browser\":"), None);
        assert_eq!(websocket_url("HTTP/1.1 200 OK\r\n\r\n"), None);
    }

    #[test]
    fn the_probe_reads_through_chunked_framing_and_a_trailing_newline() {
        let answer = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                      1a\r\n{\"webSocketDebuggerUrl\":\"ws://x\"}\r\n0\r\n\r\n";

        assert_eq!(websocket_url(answer).as_deref(), Some("ws://x"));
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_is_retried_until_the_browser_answers() {
        let root = test_root();
        let host = ScriptedHost::new(vec![
            None,
            None,
            Some("ws://127.0.0.1:9222/devtools/browser/late".to_string()),
        ]);

        let browser =
            ManagedBrowser::launch_with(&host, &root, Path::new("/opt/chrome"), &launch_settings())
                .await
                .unwrap();

        assert_eq!(
            browser.ws_endpoint(),
            "ws://127.0.0.1:9222/devtools/browser/late"
        );
        browser.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test(start_paused = true)]
    async fn a_browser_that_never_reports_an_endpoint_is_killed_and_its_profile_removed() {
        let root = test_root();
        let host = ScriptedHost::new(Vec::new());

        let failed = ManagedBrowser::launch_with(
            &host,
            &root,
            Path::new("/opt/chrome"),
            &launch_settings(),
        )
        .await;

        let Err(message) = failed else {
            panic!("a browser that never came up must fail the launch");
        };
        assert!(message.contains("did not expose a CDP endpoint"), "got {message}");
        assert!(host.killed(), "the half-started browser must be killed");
        let (_, args) = host.spawned();
        assert!(
            !user_data_dir(&args).exists(),
            "a failed launch must not leave its temporary profile behind"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_launch_failure_names_the_executable() {
        // Through the real host, pointed at a program that does not exist —
        // spawning is all that is attempted, and nothing is downloaded or
        // started.
        let host = HostBrowser;

        let failed = host.spawn(Path::new("/nonexistent/browser-for-tests"), &[]).await;

        let Err(message) = failed else {
            panic!("a missing browser must fail to spawn");
        };
        assert!(message.contains("nonexistent/browser-for-tests"), "{message}");
    }
}
