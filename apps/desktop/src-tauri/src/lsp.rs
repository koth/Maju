use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageServerSpec {
    pub language_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspServerStatus {
    pub language_id: String,
    pub configured: bool,
    pub enabled: bool,
    pub available: bool,
    pub running: bool,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspDiagnostic {
    pub path: String,
    pub message: String,
    pub severity: u32,
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

#[derive(Clone, Default)]
pub struct LspService {
    inner: Arc<Mutex<LspServiceInner>>,
}

#[derive(Default)]
struct LspServiceInner {
    servers: HashMap<WorkspaceLanguageKey, ManagedLanguageServer>,
    specs: LanguageServerRegistry,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WorkspaceLanguageKey {
    workspace_root: PathBuf,
    language_id: String,
}

struct ManagedLanguageServer {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    open_documents: HashSet<PathBuf>,
    documents: HashMap<PathBuf, LspDocumentState>,
    pending_requests: HashMap<u64, mpsc::Sender<Value>>,
    next_request_id: u64,
    status: LspServerStatus,
}

struct LspDocumentState {
    uri: String,
    version: i32,
    diagnostics: Vec<LspDiagnostic>,
}

#[derive(Clone)]
pub struct LanguageServerRegistry {
    specs: Vec<LanguageServerSpec>,
}

impl LspService {
    pub fn new() -> Self {
        Self::with_registry(LanguageServerRegistry::default())
    }

    pub fn with_registry(registry: LanguageServerRegistry) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LspServiceInner {
                servers: HashMap::new(),
                specs: registry,
            })),
        }
    }

    pub fn open_document(
        &self,
        workspace_root: &Path,
        language_id: &str,
        document_path: &Path,
        content: &str,
    ) -> Result<LspServerStatus, String> {
        let Some(spec) = self.spec_for_language(language_id) else {
            return Ok(LspServerStatus {
                language_id: language_id.to_string(),
                configured: false,
                enabled: false,
                available: false,
                running: false,
                message: None,
            });
        };
        if !spec.enabled {
            return Ok(LspServerStatus {
                language_id: language_id.to_string(),
                configured: true,
                enabled: false,
                available: false,
                running: false,
                message: Some("Language server disabled".into()),
            });
        }

        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let key = WorkspaceLanguageKey {
            workspace_root: workspace_root.clone(),
            language_id: language_id.to_string(),
        };
        let document_path = resolve_document_path(&workspace_root, document_path);

        let mut guard = self.inner.lock().map_err(|e| e.to_string())?;
        if let Some(server) = guard.servers.get_mut(&key) {
            if child_is_running(server.child.as_mut()) {
                server.did_open_document(language_id, &document_path, content)?;
                server.status.running = true;
                return Ok(server.status.clone());
            }
            guard.servers.remove(&key);
        }

        let resolved_command = match resolve_command(&spec.command) {
            Some(command) => command,
            None => {
                let status = LspServerStatus {
                    language_id: language_id.to_string(),
                    configured: true,
                    enabled: true,
                    available: false,
                    running: false,
                    message: Some(format!(
                        "Language server command not found: {}",
                        spec.command
                    )),
                };
                guard.servers.insert(
                    key,
                    ManagedLanguageServer {
                        child: None,
                        stdin: None,
                        open_documents: HashSet::new(),
                        documents: HashMap::new(),
                        pending_requests: HashMap::new(),
                        next_request_id: 1,
                        status: status.clone(),
                    },
                );
                return Ok(status);
            }
        };

        let mut command = Command::new(resolved_command);
        command
            .args(&spec.args)
            .current_dir(&workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console_window(&mut command);

        let mut child = command.spawn().map_err(|e| {
            format!(
                "Failed to start language server {} for {}: {e}",
                spec.command, language_id
            )
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let status = LspServerStatus {
            language_id: language_id.to_string(),
            configured: true,
            enabled: true,
            available: true,
            running: true,
            message: None,
        };
        let mut server = ManagedLanguageServer {
            child: Some(child),
            stdin,
            open_documents: HashSet::new(),
            documents: HashMap::new(),
            pending_requests: HashMap::new(),
            next_request_id: 1,
            status: status.clone(),
        };
        server.initialize(&workspace_root)?;
        server.did_open_document(language_id, &document_path, content)?;
        guard.servers.insert(key.clone(), server);
        if let Some(stdout) = stdout {
            spawn_lsp_reader(self.inner.clone(), key, stdout);
        }
        Ok(status)
    }

    pub fn change_document(
        &self,
        workspace_root: &Path,
        language_id: &str,
        document_path: &Path,
        content: &str,
    ) -> Result<i32, String> {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let key = WorkspaceLanguageKey {
            workspace_root: workspace_root.clone(),
            language_id: language_id.to_string(),
        };
        let document_path = resolve_document_path(&workspace_root, document_path);
        let mut guard = self.inner.lock().map_err(|e| e.to_string())?;
        let Some(server) = guard.servers.get_mut(&key) else {
            return Ok(0);
        };
        let version = server.did_change_document(document_path, content)?;
        Ok(version)
    }

    pub fn save_document(
        &self,
        workspace_root: &Path,
        language_id: &str,
        document_path: &Path,
        content: &str,
    ) -> Result<(), String> {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let key = WorkspaceLanguageKey {
            workspace_root: workspace_root.clone(),
            language_id: language_id.to_string(),
        };
        let document_path = resolve_document_path(&workspace_root, document_path);
        let mut guard = self.inner.lock().map_err(|e| e.to_string())?;
        let Some(server) = guard.servers.get_mut(&key) else {
            return Ok(());
        };
        server.did_save_document(document_path, content)
    }

    pub fn close_document(&self, workspace_root: &Path, language_id: &str, document_path: &Path) {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let key = WorkspaceLanguageKey {
            workspace_root: workspace_root.clone(),
            language_id: language_id.to_string(),
        };
        let document_path = resolve_document_path(&workspace_root, document_path);

        if let Ok(mut guard) = self.inner.lock() {
            let should_shutdown = match guard.servers.get_mut(&key) {
                Some(server) => {
                    let _ = server.did_close_document(&document_path);
                    server.open_documents.is_empty()
                }
                None => false,
            };
            if should_shutdown {
                if let Some(mut server) = guard.servers.remove(&key) {
                    server.shutdown();
                }
            }
        }
    }

    pub fn shutdown_workspace(&self, workspace_root: &Path) {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        if let Ok(mut guard) = self.inner.lock() {
            let keys = guard
                .servers
                .keys()
                .filter(|key| key.workspace_root == workspace_root)
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(mut server) = guard.servers.remove(&key) {
                    server.shutdown();
                }
            }
        }
    }

    pub fn shutdown_all(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            for (_, mut server) in guard.servers.drain() {
                server.shutdown();
            }
        }
    }

    pub fn active_server_count(&self) -> usize {
        self.inner
            .lock()
            .map(|guard| {
                guard
                    .servers
                    .values()
                    .filter(|server| server.child.is_some())
                    .count()
            })
            .unwrap_or(0)
    }

    pub fn is_language_running(&self, language_id: &str) -> bool {
        self.inner
            .lock()
            .map(|guard| {
                guard.servers.iter().any(|(key, server)| {
                    key.language_id == language_id
                        && server.status.running
                        && server.child.is_some()
                })
            })
            .unwrap_or(false)
    }

    pub fn configure_registry(&self, registry: LanguageServerRegistry) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.specs = registry;
        }
    }

    pub fn shutdown_language(&self, language_id: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            let keys = guard
                .servers
                .keys()
                .filter(|key| key.language_id == language_id)
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(mut server) = guard.servers.remove(&key) {
                    server.shutdown();
                }
            }
        }
    }

    pub fn diagnostics_for_document(
        &self,
        workspace_root: &Path,
        language_id: &str,
        document_path: &Path,
    ) -> Vec<LspDiagnostic> {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let key = WorkspaceLanguageKey {
            workspace_root: workspace_root.clone(),
            language_id: language_id.to_string(),
        };
        let document_path = resolve_document_path(&workspace_root, document_path);

        self.inner
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .servers
                    .get(&key)
                    .and_then(|server| server.documents.get(&document_path))
                    .map(|document| document.diagnostics.clone())
            })
            .unwrap_or_default()
    }

    pub fn request(
        &self,
        workspace_root: &Path,
        language_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let key = WorkspaceLanguageKey {
            workspace_root,
            language_id: language_id.to_string(),
        };
        let receiver = {
            let mut guard = self.inner.lock().map_err(|e| e.to_string())?;
            let Some(server) = guard.servers.get_mut(&key) else {
                return Ok(Value::Null);
            };
            if !server.status.running {
                return Ok(Value::Null);
            }
            server.send_request(method, params)?
        };

        let response = receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| format!("LSP request timed out: {method}"))?;
        if let Some(error) = response.get("error") {
            return Err(format!("LSP request failed: {error}"));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    fn spec_for_language(&self, language_id: &str) -> Option<LanguageServerSpec> {
        self.inner
            .lock()
            .ok()?
            .specs
            .spec_for_language(language_id)
            .cloned()
    }
}

impl Drop for LspService {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            self.shutdown_all();
        }
    }
}

impl LanguageServerRegistry {
    pub fn new(specs: Vec<LanguageServerSpec>) -> Self {
        Self { specs }
    }

    pub fn spec_for_language(&self, language_id: &str) -> Option<&LanguageServerSpec> {
        self.specs
            .iter()
            .find(|spec| spec.language_id == language_id)
    }

    pub fn from_settings(settings: &workspace_model::AppSettings) -> Self {
        Self::new(
            app_core::settings::all_effective_lsp_servers(settings)
                .into_iter()
                .map(LanguageServerSpec::from)
                .collect(),
        )
    }
}

impl Default for LanguageServerSpec {
    fn default() -> Self {
        Self {
            language_id: "plaintext".into(),
            command: String::new(),
            args: Vec::new(),
            enabled: false,
        }
    }
}

impl From<app_core::settings::EffectiveLspServerConfig> for LanguageServerSpec {
    fn from(config: app_core::settings::EffectiveLspServerConfig) -> Self {
        Self {
            language_id: config.language_id,
            command: config.command,
            args: config.args,
            enabled: config.enabled,
        }
    }
}

impl Default for LanguageServerRegistry {
    fn default() -> Self {
        Self::new(
            app_core::settings::all_effective_lsp_servers(&workspace_model::AppSettings {
                selected_agent: workspace_model::AgentCliId::Codebuddy,
                acp_port: 0,
                theme: workspace_model::AppTheme::KodexDark,
                lsp_servers: std::collections::BTreeMap::new(),
                codex_connection_mode: workspace_model::CodexConnectionMode::Managed,
                selected_codex_provider_profile_id: None,
                selected_claude_provider_profile_id: None,
                claude_woa: workspace_model::ClaudeWoaSettings::default(),
            })
            .into_iter()
            .map(LanguageServerSpec::from)
            .collect(),
        )
    }
}

impl ManagedLanguageServer {
    fn initialize(&mut self, workspace_root: &Path) -> Result<(), String> {
        let id = self.next_id();
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": path_to_uri(workspace_root),
                "capabilities": {
                    "textDocument": {
                        "synchronization": {
                            "didSave": true
                        },
                        "publishDiagnostics": {
                            "relatedInformation": true
                        },
                        "hover": {
                            "contentFormat": ["markdown", "plaintext"]
                        },
                        "completion": {
                            "completionItem": {
                                "snippetSupport": false
                            }
                        }
                    },
                    "workspace": {
                        "workspaceFolders": false
                    }
                }
            }
        }))?;
        self.write_notification("initialized", json!({}))
    }

    fn did_open_document(
        &mut self,
        language_id: &str,
        document_path: &Path,
        content: &str,
    ) -> Result<(), String> {
        let document_path = normalize_document_path(document_path);
        if self.open_documents.contains(&document_path) {
            return Ok(());
        }
        let uri = path_to_uri(&document_path);
        let version = 1;
        self.open_documents.insert(document_path.clone());
        self.documents.insert(
            document_path.clone(),
            LspDocumentState {
                uri: uri.clone(),
                version,
                diagnostics: Vec::new(),
            },
        );
        self.write_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": version,
                    "text": content,
                }
            }),
        )
    }

    fn did_change_document(
        &mut self,
        document_path: PathBuf,
        content: &str,
    ) -> Result<i32, String> {
        let Some(document) = self.documents.get_mut(&document_path) else {
            return Ok(0);
        };
        document.version += 1;
        let version = document.version;
        let uri = document.uri.clone();
        self.write_notification(
            "textDocument/didChange",
            json!({
                "textDocument": {
                    "uri": uri,
                    "version": version,
                },
                "contentChanges": [
                    { "text": content }
                ]
            }),
        )?;
        Ok(version)
    }

    fn did_save_document(&mut self, document_path: PathBuf, content: &str) -> Result<(), String> {
        let Some(document) = self.documents.get(&document_path) else {
            return Ok(());
        };
        self.write_notification(
            "textDocument/didSave",
            json!({
                "textDocument": {
                    "uri": document.uri,
                },
                "text": content,
            }),
        )
    }

    fn did_close_document(&mut self, document_path: &Path) -> Result<(), String> {
        let document_path = normalize_document_path(document_path);
        self.open_documents.remove(&document_path);
        let Some(document) = self.documents.remove(&document_path) else {
            return Ok(());
        };
        self.write_notification(
            "textDocument/didClose",
            json!({
                "textDocument": {
                    "uri": document.uri,
                }
            }),
        )
    }

    fn shutdown(&mut self) {
        let shutdown_id = self.next_id();
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": shutdown_id,
            "method": "shutdown",
            "params": null,
        }));
        let _ = self.write_notification("exit", json!(null));
        self.stdin.take();
        self.pending_requests.clear();
        if let Some(mut child) = self.child.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        self.status.running = false;
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    fn write_notification(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn write_message(&mut self, value: &Value) -> Result<(), String> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Ok(());
        };
        let payload = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        write!(stdin, "Content-Length: {}\r\n\r\n", payload.len()).map_err(|e| e.to_string())?;
        stdin.write_all(&payload).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())
    }

    fn send_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<mpsc::Receiver<Value>, String> {
        if self.stdin.is_none() {
            return Err(format!(
                "Language server is not writable for request: {method}"
            ));
        }
        let id = self.next_id();
        let (sender, receiver) = mpsc::channel();
        self.pending_requests.insert(id, sender);
        if let Err(error) = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })) {
            self.pending_requests.remove(&id);
            return Err(error);
        }
        Ok(receiver)
    }
}

fn child_is_running(child: Option<&mut Child>) -> bool {
    let Some(child) = child else {
        return false;
    };
    child.try_wait().ok().flatten().is_none()
}

fn normalize_document_path(path: &Path) -> PathBuf {
    path.components().collect()
}

fn resolve_document_path(workspace_root: &Path, document_path: &Path) -> PathBuf {
    let path = if document_path.is_absolute() {
        document_path.to_path_buf()
    } else {
        workspace_root.join(document_path)
    };
    path.canonicalize()
        .unwrap_or_else(|_| normalize_document_path(&path))
}

fn path_to_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn spawn_lsp_reader(
    inner: Arc<Mutex<LspServiceInner>>,
    key: WorkspaceLanguageKey,
    stdout: ChildStdout,
) {
    std::thread::spawn(move || {
        let mut reader = stdout;
        while let Ok(Some(message)) = read_lsp_message(&mut reader) {
            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                if let Ok(mut guard) = inner.lock() {
                    if let Some(server) = guard.servers.get_mut(&key) {
                        if let Some(sender) = server.pending_requests.remove(&id) {
                            let _ = sender.send(message);
                            continue;
                        }
                    }
                }
            }

            if message.get("method").and_then(Value::as_str)
                == Some("textDocument/publishDiagnostics")
            {
                if let Some((uri, diagnostics)) = parse_publish_diagnostics(&message) {
                    if let Ok(mut guard) = inner.lock() {
                        if let Some(server) = guard.servers.get_mut(&key) {
                            for document in server.documents.values_mut() {
                                if document.uri == uri {
                                    document.diagnostics = diagnostics
                                        .iter()
                                        .map(|diagnostic| LspDiagnostic {
                                            path: uri_to_path(&uri),
                                            message: diagnostic.message.clone(),
                                            severity: diagnostic.severity,
                                            start_line: diagnostic.start_line,
                                            start_character: diagnostic.start_character,
                                            end_line: diagnostic.end_line,
                                            end_character: diagnostic.end_character,
                                        })
                                        .collect();
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Ok(mut guard) = inner.lock() {
            if let Some(server) = guard.servers.get_mut(&key) {
                for document in server.documents.values_mut() {
                    document.diagnostics.clear();
                }
                server.status.running = false;
            }
        }
    });
}

fn read_lsp_message<R: Read>(reader: &mut R) -> Result<Option<Value>, String> {
    let mut header = Vec::new();
    let mut window = [0_u8; 4];
    loop {
        let mut byte = [0_u8; 1];
        match reader.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                header.push(byte[0]);
                window.rotate_left(1);
                window[3] = byte[0];
                if window == *b"\r\n\r\n" {
                    break;
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }

    let header = String::from_utf8_lossy(&header);
    let content_length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .ok_or_else(|| "LSP message missing Content-Length".to_string())?;
    let mut payload = vec![0_u8; content_length];
    reader.read_exact(&mut payload).map_err(|e| e.to_string())?;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[derive(Clone)]
struct ParsedDiagnostic {
    message: String,
    severity: u32,
    start_line: u32,
    start_character: u32,
    end_line: u32,
    end_character: u32,
}

fn parse_publish_diagnostics(message: &Value) -> Option<(String, Vec<ParsedDiagnostic>)> {
    let params = message.get("params")?;
    let uri = params.get("uri")?.as_str()?.to_string();
    let diagnostics = params
        .get("diagnostics")?
        .as_array()?
        .iter()
        .filter_map(|diagnostic| {
            let range = diagnostic.get("range")?;
            let start = range.get("start")?;
            let end = range.get("end")?;
            Some(ParsedDiagnostic {
                message: diagnostic.get("message")?.as_str()?.to_string(),
                severity: diagnostic
                    .get("severity")
                    .and_then(Value::as_u64)
                    .unwrap_or(1) as u32,
                start_line: start.get("line")?.as_u64()? as u32,
                start_character: start.get("character")?.as_u64()? as u32,
                end_line: end.get("line")?.as_u64()? as u32,
                end_character: end.get("character")?.as_u64()? as u32,
            })
        })
        .collect();
    Some((uri, diagnostics))
}

fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file:///")
        .or_else(|| uri.strip_prefix("file://"))
        .unwrap_or(uri)
        .replace('/', std::path::MAIN_SEPARATOR_STR)
}

fn resolve_command(command: &str) -> Option<PathBuf> {
    let command_path = PathBuf::from(command);
    if command_path.components().count() > 1 {
        return command_path.exists().then_some(command_path);
    }

    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for candidate in command_candidates(&dir, command) {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn probe_command(command: &str) -> workspace_model::LspProbeResult {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return workspace_model::LspProbeResult {
            available: false,
            resolved_path: None,
            message: Some("Command is empty".into()),
        };
    }

    match resolve_command(trimmed) {
        Some(path) => workspace_model::LspProbeResult {
            available: true,
            resolved_path: Some(path),
            message: None,
        },
        None => workspace_model::LspProbeResult {
            available: false,
            resolved_path: None,
            message: Some(format!("Language server command not found: {trimmed}")),
        },
    }
}

fn command_candidates(dir: &Path, command: &str) -> Vec<PathBuf> {
    let mut candidates = vec![dir.join(command)];
    #[cfg(windows)]
    {
        if Path::new(command).extension().is_none() {
            let extensions = std::env::var_os("PATHEXT")
                .map(|value| {
                    value
                        .to_string_lossy()
                        .split(';')
                        .filter(|ext| !ext.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| {
                    vec![".COM".into(), ".EXE".into(), ".BAT".into(), ".CMD".into()]
                });
            candidates.extend(
                extensions
                    .into_iter()
                    .map(|ext| dir.join(format!("{command}{ext}"))),
            );
        }
    }
    candidates
}

#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn missing_language_server_degrades_gracefully() {
        let service =
            LspService::with_registry(LanguageServerRegistry::new(vec![LanguageServerSpec {
                language_id: "madeup".into(),
                command: "kodex-definitely-missing-lsp-command".into(),
                args: Vec::new(),
                enabled: true,
            }]));

        let status = service
            .open_document(Path::new("."), "madeup", Path::new("missing.fake"), "")
            .expect("missing server should not be fatal");

        assert!(!status.available);
        assert!(!status.running);
        assert_eq!(service.active_server_count(), 0);
    }

    #[test]
    fn disabled_language_server_degrades_quietly() {
        let service =
            LspService::with_registry(LanguageServerRegistry::new(vec![LanguageServerSpec {
                language_id: "rust".into(),
                command: "rust-analyzer".into(),
                args: Vec::new(),
                enabled: false,
            }]));

        let status = service
            .open_document(Path::new("."), "rust", Path::new("src/main.rs"), "")
            .expect("disabled server should not be fatal");

        assert!(status.configured);
        assert!(!status.enabled);
        assert!(!status.available);
        assert!(!status.running);
        assert_eq!(service.active_server_count(), 0);
    }

    #[test]
    fn unconfigured_language_degrades_gracefully() {
        let service = LspService::with_registry(LanguageServerRegistry::new(vec![]));

        let status = service
            .open_document(
                Path::new("."),
                "unknown-language",
                Path::new("unknown.fake"),
                "",
            )
            .expect("unconfigured language should not be fatal");

        assert!(!status.available);
        assert!(!status.running);
        assert!(!status.configured);
        assert_eq!(status.message.as_deref(), None);
    }

    #[test]
    fn document_versions_increment_and_close_clears_state() {
        let document = PathBuf::from("src/main.rs");
        let uri = path_to_uri(&document);
        let mut server = ManagedLanguageServer {
            child: None,
            stdin: None,
            open_documents: HashSet::from([document.clone()]),
            documents: HashMap::from([(
                document.clone(),
                LspDocumentState {
                    uri,
                    version: 1,
                    diagnostics: vec![LspDiagnostic {
                        path: "src/main.rs".into(),
                        message: "stale".into(),
                        severity: 1,
                        start_line: 0,
                        start_character: 0,
                        end_line: 0,
                        end_character: 1,
                    }],
                },
            )]),
            pending_requests: HashMap::new(),
            next_request_id: 1,
            status: LspServerStatus {
                language_id: "rust".into(),
                configured: true,
                enabled: true,
                available: true,
                running: true,
                message: None,
            },
        };

        assert_eq!(
            server
                .did_change_document(document.clone(), "fn main() {}")
                .unwrap(),
            2
        );
        assert_eq!(
            server
                .did_change_document(document.clone(), "fn main() { }")
                .unwrap(),
            3
        );

        server.did_close_document(&document).unwrap();

        assert!(server.open_documents.is_empty());
        assert!(!server.documents.contains_key(&document));
    }

    #[test]
    fn parses_lsp_messages_and_publish_diagnostics() {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///workspace/src/main.rs",
                "diagnostics": [
                    {
                        "message": "expected semicolon",
                        "severity": 1,
                        "range": {
                            "start": { "line": 2, "character": 4 },
                            "end": { "line": 2, "character": 9 }
                        }
                    }
                ]
            }
        });
        let body = serde_json::to_vec(&payload).unwrap();
        let mut raw = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        raw.extend(body);

        let parsed = read_lsp_message(&mut Cursor::new(raw))
            .expect("message should parse")
            .expect("message should exist");
        let (uri, diagnostics) =
            parse_publish_diagnostics(&parsed).expect("diagnostics should parse");

        assert_eq!(uri, "file:///workspace/src/main.rs");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "expected semicolon");
        assert_eq!(diagnostics[0].start_line, 2);
        assert_eq!(diagnostics[0].end_character, 9);
    }

    #[test]
    fn resolves_workspace_relative_documents_to_absolute_paths() {
        let workspace = std::env::current_dir().expect("current dir");
        let relative = Path::new("src/main.rs");

        let resolved = resolve_document_path(&workspace, relative);

        assert!(resolved.is_absolute());
        assert!(path_to_uri(&resolved).starts_with("file:///"));
    }

    #[test]
    fn probe_command_uses_runtime_resolution_without_spawning() {
        let missing = probe_command("kodex-definitely-missing-lsp-command");
        let empty = probe_command("   ");

        assert!(!missing.available);
        assert!(missing.message.unwrap().contains("not found"));
        assert!(!empty.available);
        assert_eq!(empty.message.as_deref(), Some("Command is empty"));
    }

    #[cfg(windows)]
    #[test]
    fn shutdown_workspace_kills_child_processes() {
        let service =
            LspService::with_registry(LanguageServerRegistry::new(vec![LanguageServerSpec {
                language_id: "test-sleep".into(),
                command: "powershell.exe".into(),
                args: vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    "Start-Sleep -Seconds 30".into(),
                ],
                enabled: true,
            }]));

        let workspace = std::env::current_dir().expect("current dir");
        let status = service
            .open_document(&workspace, "test-sleep", Path::new("file.test"), "hello")
            .expect("test server should start");

        assert!(status.available);
        assert!(status.running);
        assert_eq!(service.active_server_count(), 1);

        service.shutdown_workspace(&workspace);

        assert_eq!(service.active_server_count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn closing_last_document_kills_child_process() {
        let service =
            LspService::with_registry(LanguageServerRegistry::new(vec![LanguageServerSpec {
                language_id: "test-sleep".into(),
                command: "powershell.exe".into(),
                args: vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    "Start-Sleep -Seconds 30".into(),
                ],
                enabled: true,
            }]));

        let workspace = std::env::current_dir().expect("current dir");
        let document = Path::new("file.test");
        service
            .open_document(&workspace, "test-sleep", document, "hello")
            .expect("test server should start");
        assert_eq!(service.active_server_count(), 1);

        service.close_document(&workspace, "test-sleep", document);

        assert_eq!(service.active_server_count(), 0);
    }
}
