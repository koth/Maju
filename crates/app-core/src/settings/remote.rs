use super::agent_cli::AGENTS;
use super::*;
use crate::remote_ssh::{
    RemoteSshCommand, RemoteSshCommandRunner, SystemRemoteSshCommandRunner, first_nonempty,
    sanitize_ssh_diagnostic,
};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use workspace_model::{
    AgentCliStatus, LspProbeResult, LspServerConfigInput, LspServerSettingsEntry,
    LspSettingsSnapshot, RemoteMachineProfile,
};

pub fn remote_settings_snapshot(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
) -> Result<AgentSettingsSnapshot> {
    remote_settings_snapshot_with_runner(profile, ssh_password, &SystemRemoteSshCommandRunner)
}

pub fn remote_select_agent(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    agent: AgentCliId,
) -> Result<AgentSettingsSnapshot> {
    remote_select_agent_with_runner(profile, ssh_password, agent, &SystemRemoteSshCommandRunner)
}

pub fn remote_select_theme(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    theme: AppTheme,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| select_theme(paths, theme),
    )
}

pub fn remote_select_agent_provider_profile(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    family: AgentProviderFamily,
    profile_id: &str,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| select_agent_provider_profile(paths, family, profile_id),
    )
}

pub fn remote_save_agent_provider_secret(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    family: AgentProviderFamily,
    profile_id: &str,
    secret: &str,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| save_agent_provider_secret(paths, family, profile_id, secret),
    )
}

pub fn remote_save_provider_models(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    provider: &str,
    models: Vec<String>,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| save_provider_models(paths, provider, models),
    )
}

pub fn remote_save_provider_models_with_model_list_url(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    provider: &str,
    models: Vec<String>,
    model_list_url: String,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| {
            save_provider_models_with_model_list_url(paths, provider, models, Some(model_list_url))
        },
    )
}

pub fn remote_reset_provider_models(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    provider: &str,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| reset_provider_models(paths, provider),
    )
}

pub fn remote_select_claude_fast_model(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    model_id: Option<String>,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| select_claude_fast_model(paths, model_id),
    )
}

pub fn remote_lsp_settings_snapshot(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
) -> Result<LspSettingsSnapshot> {
    remote_lsp_settings_snapshot_with_runner(profile, ssh_password, &SystemRemoteSshCommandRunner)
}

pub fn remote_save_lsp_server_config(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    config: LspServerConfigInput,
) -> Result<LspSettingsSnapshot> {
    remote_update_lsp_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| save_lsp_server_config(paths, config).map(|_| ()),
    )
}

pub fn remote_reset_lsp_server_config(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    language_id: &str,
) -> Result<LspSettingsSnapshot> {
    remote_update_lsp_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| reset_lsp_server_config(paths, language_id).map(|_| ()),
    )
}

pub fn remote_probe_lsp_server(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    command: &str,
) -> Result<LspProbeResult> {
    remote_probe_lsp_server_with_runner(
        profile,
        ssh_password,
        command,
        &SystemRemoteSshCommandRunner,
    )
}

const REMOTE_SETTINGS_TIMEOUT: Duration = Duration::from_secs(12);
const REMOTE_SETTINGS_WRITE_TIMEOUT: Duration = Duration::from_secs(20);
const REMOTE_SETTINGS_FILES: &[&str] = &[
    "config/settings.json",
    "config/provider-secrets.json",
    "config/provider-models.json",
    "config.toml",
];

#[derive(Debug, Clone, Deserialize)]
struct RemoteSettingsExport {
    home: String,
    files: BTreeMap<String, Option<String>>,
    agents: BTreeMap<String, Option<String>>,
    env_override: Option<String>,
}

struct RemoteSettingsMirror {
    paths: AppPaths,
    temp_root: PathBuf,
    export: RemoteSettingsExport,
}

impl Drop for RemoteSettingsMirror {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.temp_root);
    }
}

pub(super) fn remote_settings_snapshot_with_runner<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
) -> Result<AgentSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
{
    let mirror = pull_remote_settings(profile, ssh_password, runner)?;
    Ok(settings_snapshot_from_remote_mirror(&mirror))
}

pub(super) fn remote_select_agent_with_runner<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    agent: AgentCliId,
    runner: &R,
) -> Result<AgentSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
{
    let mut mirror = pull_remote_settings(profile, ssh_password, runner)?;
    let status = remote_agent_statuses(&mirror.export, agent)
        .into_iter()
        .find(|status| status.id == agent)
        .ok_or_else(|| anyhow!("Unsupported agent"))?;
    if !status.installed {
        anyhow::bail!("{} is not installed on remote", status.binary);
    }
    let mut settings = load_app_settings(&mirror.paths);
    settings.selected_agent = agent;
    save_app_settings(&mirror.paths, &settings)?;
    push_remote_settings(profile, ssh_password, runner, &mirror.paths)?;
    mirror.export = pull_remote_settings_export(profile, ssh_password, runner)?;
    Ok(settings_snapshot_from_remote_mirror(&mirror))
}

pub(super) fn remote_update_settings_with_runner<R, F>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
    update: F,
) -> Result<AgentSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
    F: FnOnce(&AppPaths) -> Result<AgentSettingsSnapshot>,
{
    let mut mirror = pull_remote_settings(profile, ssh_password, runner)?;
    let _ = update(&mirror.paths)?;
    push_remote_settings(profile, ssh_password, runner, &mirror.paths)?;
    mirror.export = pull_remote_settings_export(profile, ssh_password, runner)?;
    Ok(settings_snapshot_from_remote_mirror(&mirror))
}

fn remote_lsp_settings_snapshot_with_runner<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
) -> Result<LspSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
{
    let mirror = pull_remote_settings(profile, ssh_password, runner)?;
    remote_lsp_snapshot_from_mirror(profile, ssh_password, runner, &mirror)
}

fn remote_update_lsp_settings_with_runner<R, F>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
    update: F,
) -> Result<LspSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
    F: FnOnce(&AppPaths) -> Result<()>,
{
    let mut mirror = pull_remote_settings(profile, ssh_password, runner)?;
    update(&mirror.paths)?;
    push_remote_settings(profile, ssh_password, runner, &mirror.paths)?;
    mirror.export = pull_remote_settings_export(profile, ssh_password, runner)?;
    remote_lsp_snapshot_from_mirror(profile, ssh_password, runner, &mirror)
}

fn pull_remote_settings<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
) -> Result<RemoteSettingsMirror>
where
    R: RemoteSshCommandRunner,
{
    let export = pull_remote_settings_export(profile, ssh_password, runner)?;
    let temp_root = unique_remote_settings_temp_root();
    let paths = AppPaths::from_root(temp_root.join(".kodex"));
    for (relative, content) in &export.files {
        let Some(content) = content else {
            continue;
        };
        let target = paths.root().join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create mirror directory {}", parent.display())
            })?;
        }
        std::fs::write(&target, content)
            .with_context(|| format!("failed to write mirror file {}", target.display()))?;
    }
    Ok(RemoteSettingsMirror {
        paths,
        temp_root,
        export,
    })
}

fn pull_remote_settings_export<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
) -> Result<RemoteSettingsExport>
where
    R: RemoteSshCommandRunner,
{
    let output = runner.run_ssh_command(&RemoteSshCommand::new(
        profile.ssh_target.clone(),
        profile.ssh_port,
        remote_settings_export_command(),
        ssh_password,
        REMOTE_SETTINGS_TIMEOUT,
    ));
    if !output.success {
        return Err(anyhow!(remote_settings_error("远程设置读取失败", &output)));
    }
    serde_json::from_str(&output.stdout)
        .with_context(|| format!("远程设置响应不是有效 JSON：{}", output.stdout.trim()))
}

fn push_remote_settings<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
    paths: &AppPaths,
) -> Result<()>
where
    R: RemoteSshCommandRunner,
{
    let mut files = BTreeMap::<String, Option<String>>::new();
    for relative in REMOTE_SETTINGS_FILES {
        let path = paths.root().join(relative);
        let content = std::fs::read_to_string(&path).ok();
        files.insert((*relative).to_string(), content);
    }
    let stdin = serde_json::to_vec(&json!({ "files": files }))?;
    let output = runner.run_ssh_command(
        &RemoteSshCommand::new(
            profile.ssh_target.clone(),
            profile.ssh_port,
            remote_settings_import_command(),
            ssh_password,
            REMOTE_SETTINGS_WRITE_TIMEOUT,
        )
        .with_stdin(stdin),
    );
    if !output.success {
        return Err(anyhow!(remote_settings_error("远程设置写入失败", &output)));
    }
    Ok(())
}

fn settings_snapshot_from_remote_mirror(mirror: &RemoteSettingsMirror) -> AgentSettingsSnapshot {
    let mut snapshot = settings_snapshot(&mirror.paths);
    snapshot.agents = remote_agent_statuses(&mirror.export, snapshot.settings.selected_agent);
    snapshot.env_override = mirror.export.env_override.clone();
    snapshot.codex_acp.config_path = remote_settings_path(&mirror.export.home, "config.toml");
    snapshot
}

fn remote_lsp_snapshot_from_mirror<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
    mirror: &RemoteSettingsMirror,
) -> Result<LspSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
{
    let settings = load_app_settings(&mirror.paths);
    let mut servers = Vec::new();
    for server in all_effective_lsp_servers(&settings) {
        let probe = if server.enabled {
            remote_probe_lsp_server_with_runner(profile, ssh_password, &server.command, runner)?
        } else {
            LspProbeResult {
                available: false,
                resolved_path: None,
                message: Some("Language server disabled".into()),
            }
        };
        servers.push(LspServerSettingsEntry {
            language_id: server.language_id,
            display_name: server.display_name,
            enabled: server.enabled,
            command: server.command,
            args: server.args,
            default_command: server.default_command,
            default_args: server.default_args,
            available: probe.available,
            resolved_path: probe.resolved_path,
            running: false,
            message: probe.message,
            customized: server.customized,
        });
    }
    Ok(LspSettingsSnapshot { servers })
}

fn remote_probe_lsp_server_with_runner<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    command: &str,
    runner: &R,
) -> Result<LspProbeResult>
where
    R: RemoteSshCommandRunner,
{
    let output = runner.run_ssh_command(&RemoteSshCommand::new(
        profile.ssh_target.clone(),
        profile.ssh_port,
        remote_lsp_probe_command(command),
        ssh_password,
        REMOTE_SETTINGS_TIMEOUT,
    ));
    if !output.success {
        return Err(anyhow!(remote_settings_error("远程 LSP 探测失败", &output)));
    }
    serde_json::from_str(&output.stdout)
        .with_context(|| format!("远程 LSP 探测响应不是有效 JSON：{}", output.stdout.trim()))
}

fn remote_agent_statuses(
    export: &RemoteSettingsExport,
    selected_agent: AgentCliId,
) -> Vec<AgentCliStatus> {
    AGENTS
        .iter()
        .map(|definition| {
            let detected_path = export
                .agents
                .get(definition.binary)
                .and_then(|path| path.as_deref())
                .filter(|path| !path.trim().is_empty())
                .map(|path| PathBuf::from(path.trim()));
            AgentCliStatus {
                id: definition.id,
                label: definition.label.to_string(),
                binary: definition.binary.to_string(),
                installed: detected_path.is_some(),
                detected_path,
                selected: definition.id == selected_agent,
            }
        })
        .collect()
}

fn unique_remote_settings_temp_root() -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "kodex-remote-settings-{}-{now}",
        std::process::id()
    ))
}

fn remote_settings_path(home: &str, relative: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/.kodex/{}",
        home.trim_end_matches('/'),
        relative.trim_start_matches('/')
    ))
}

fn remote_settings_error(prefix: &str, output: &crate::remote_ssh::RemoteSshOutput) -> String {
    if output.timed_out {
        return format!("{prefix}：SSH 命令超时");
    }
    let message = first_nonempty(&output.stderr, &output.stdout)
        .map(sanitize_ssh_diagnostic)
        .unwrap_or_else(|| "SSH 命令失败但没有输出".into());
    format!("{prefix}：{message}")
}

fn remote_settings_export_command() -> String {
    format!(
        "node -e {}",
        shell_words::quote(
            r#"
const fs = require('fs');
const path = require('path');
const cp = require('child_process');
const os = require('os');
const home = process.env.HOME || os.homedir();
const root = path.join(home, '.kodex');
const rels = ['config/settings.json', 'config/provider-secrets.json', 'config/provider-models.json', 'config.toml'];
function read(rel) {
  try { return fs.readFileSync(path.join(root, rel), 'utf8'); }
  catch (error) {
    if (error && error.code === 'ENOENT') return null;
    throw error;
  }
}
function which(binary) {
  const result = cp.spawnSync('sh', ['-lc', `command -v ${binary} 2>/dev/null || true`], { encoding: 'utf8' });
  return (result.stdout || '').trim() || null;
}
const files = {};
for (const rel of rels) files[rel] = read(rel);
const agents = {
  'claude-agent-acp': which('claude-agent-acp'),
  'codex-acp': which('codex-acp'),
  'codebuddy': which('codebuddy')
};
if (files['config.toml'] == null) files['config.toml'] = read('config/config.toml');
console.log(JSON.stringify({ home, files, agents, env_override: process.env.ACP_AGENT_COMMAND || null }));
"#,
        )
    )
}

fn remote_settings_import_command() -> String {
    format!(
        "node -e {}",
        shell_words::quote(
            r#"
const fs = require('fs');
const path = require('path');
const os = require('os');
const home = process.env.HOME || os.homedir();
const root = path.join(home, '.kodex');
const chunks = [];
process.stdin.on('data', (chunk) => chunks.push(chunk));
process.stdin.on('end', () => {
  const payload = JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}');
  const files = payload.files || {};
  for (const [rel, content] of Object.entries(files)) {
    if (content == null) continue;
    if (path.isAbsolute(rel) || rel.split(/[\\/]+/).includes('..')) throw new Error('invalid settings path');
    const target = path.join(root, rel);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, String(content), 'utf8');
  }
  console.log(JSON.stringify({ ok: true }));
});
"#,
        )
    )
}

fn remote_lsp_probe_command(command: &str) -> String {
    format!(
        "node -e {} -- {}",
        shell_words::quote(
            r#"
const fs = require('fs');
const cp = require('child_process');
const command = process.argv[1] || '';
const binary = command.trim().split(/\s+/)[0] || '';
if (!binary) {
  console.log(JSON.stringify({ available: false, resolvedPath: null, message: 'Command is empty' }));
  process.exit(0);
}
let resolvedPath = null;
if (binary.includes('/')) {
  try {
    const stat = fs.statSync(binary);
    if (stat.isFile()) resolvedPath = binary;
  } catch (_) {}
} else {
  const escaped = binary.replace(/'/g, `'\\''`);
  const result = cp.spawnSync('sh', ['-lc', `command -v '${escaped}' 2>/dev/null || true`], { encoding: 'utf8' });
  resolvedPath = (result.stdout || '').trim() || null;
}
console.log(JSON.stringify({
  available: !!resolvedPath,
  resolvedPath,
  message: resolvedPath ? null : `${binary} not found on remote PATH`
}));
"#,
        ),
        shell_words::quote(command)
    )
}
