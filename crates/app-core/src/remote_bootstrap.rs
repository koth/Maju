use crate::remote_ssh::{
    RemoteSshCommand, RemoteSshCommandRunner, first_nonempty, sanitize_ssh_diagnostic,
};
use anyhow::{Result, anyhow};
use std::time::Duration;
use uuid::Uuid;
use workspace_model::{
    AgentCliId, RemoteMachineProfile, RemoteOpenPhaseKind, RemoteOpenPhaseStatus,
    RemoteOpenProgressEvent,
};

const SSH_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const PLATFORM_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const RUNTIME_PREPARE_TIMEOUT: Duration = Duration::from_secs(10);
const AGENT_INSTALL_TIMEOUT: Duration = Duration::from_secs(240);
const AGENT_VERIFY_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_RUNTIME_ROOT: &str = ".kodex/remote-agents";

pub struct RemoteAgentBootstrapRequest<'a> {
    pub request_id: Uuid,
    pub profile: &'a RemoteMachineProfile,
    pub remote_path: &'a str,
    pub ssh_password: Option<&'a str>,
    pub agent_cli: AgentCliId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAgentBootstrapResult {
    pub agent_command: String,
    pub agent_env: Vec<(String, String)>,
    pub platform: RemotePlatform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePlatform {
    pub os: String,
    pub arch: String,
    pub home: String,
    pub missing_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct AgentBootstrapStrategy {
    agent: AgentCliId,
    binary: &'static str,
    install: AgentInstallStrategy,
    args: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
enum AgentInstallStrategy {
    GithubReleaseArchive {
        repo: &'static str,
        url_env: &'static str,
        linux_x64_asset_glob: &'static str,
        linux_arm64_asset_glob: &'static str,
    },
    NpmPackage {
        package: &'static str,
    },
}

const AGENT_STRATEGIES: &[AgentBootstrapStrategy] = &[
    AgentBootstrapStrategy {
        agent: AgentCliId::CodexAcp,
        binary: "codex-acp",
        install: AgentInstallStrategy::GithubReleaseArchive {
            repo: "koth/Kodex",
            url_env: "KODEX_REMOTE_CODEX_ACP_ARCHIVE_URL",
            linux_x64_asset_glob: "codex-acp-linux-x64.tar.gz",
            linux_arm64_asset_glob: "codex-acp-linux-arm64.tar.gz",
        },
        args: &[],
    },
    AgentBootstrapStrategy {
        agent: AgentCliId::ClaudeAgentAcp,
        binary: "claude-agent-acp",
        install: AgentInstallStrategy::GithubReleaseArchive {
            repo: "koth/Kodex",
            url_env: "KODEX_REMOTE_CLAUDE_AGENT_ACP_ARCHIVE_URL",
            linux_x64_asset_glob: "kodex-claude-linux-x64.tar.gz",
            linux_arm64_asset_glob: "kodex-claude-linux-arm64.tar.gz",
        },
        args: &[],
    },
    AgentBootstrapStrategy {
        agent: AgentCliId::Codebuddy,
        binary: "codebuddy",
        install: AgentInstallStrategy::NpmPackage {
            package: "@tencent-ai/codebuddy-code",
        },
        args: &["--acp", "--acp-transport", "streamable-http"],
    },
];

pub fn bootstrap_remote_agent<R, F>(
    request: RemoteAgentBootstrapRequest<'_>,
    runner: &R,
    mut progress: F,
) -> Result<RemoteAgentBootstrapResult>
where
    R: RemoteSshCommandRunner,
    F: FnMut(RemoteOpenProgressEvent),
{
    let strategy = strategy(request.agent_cli)
        .ok_or_else(|| anyhow!("Selected agent is not supported for remote Linux bootstrap"))?;

    emit(
        request.request_id,
        RemoteOpenPhaseKind::Ssh,
        RemoteOpenPhaseStatus::Running,
        0,
        Some("正在连接远程机器".into()),
        &mut progress,
    );
    let ssh = run_remote_command(&request, "printf kodex-ssh-ok", SSH_PROBE_TIMEOUT, runner);
    if !ssh.success {
        let message = output_error("SSH 连接失败", &ssh);
        emit(
            request.request_id,
            RemoteOpenPhaseKind::Ssh,
            RemoteOpenPhaseStatus::Failed,
            ssh.elapsed_ms,
            Some(message.clone()),
            &mut progress,
        );
        return Err(anyhow!(message));
    }
    emit(
        request.request_id,
        RemoteOpenPhaseKind::Ssh,
        RemoteOpenPhaseStatus::Succeeded,
        ssh.elapsed_ms,
        Some("SSH 已连接".into()),
        &mut progress,
    );

    let platform = detect_remote_platform(&request, strategy, runner, &mut progress)?;
    validate_remote_path(&request, runner, &mut progress)?;
    let runtime_base = prepare_runtime_directory(&request, strategy, runner, &mut progress)?;
    install_or_reuse_agent(
        &request,
        strategy,
        &platform,
        &runtime_base,
        runner,
        &mut progress,
    )?;
    let command = verify_agent(&request, strategy, &runtime_base, runner, &mut progress)?;

    Ok(RemoteAgentBootstrapResult {
        agent_command: command,
        agent_env: Vec::new(),
        platform,
    })
}

pub fn agent_strategy_package(agent: AgentCliId) -> Option<&'static str> {
    strategy(agent).and_then(|strategy| match strategy.install {
        AgentInstallStrategy::NpmPackage { package } => Some(package),
        AgentInstallStrategy::GithubReleaseArchive { .. } => None,
    })
}

fn strategy(agent: AgentCliId) -> Option<AgentBootstrapStrategy> {
    AGENT_STRATEGIES
        .iter()
        .copied()
        .find(|strategy| strategy.agent == agent)
}

fn detect_remote_platform<R, F>(
    request: &RemoteAgentBootstrapRequest<'_>,
    strategy: AgentBootstrapStrategy,
    runner: &R,
    progress: &mut F,
) -> Result<RemotePlatform>
where
    R: RemoteSshCommandRunner,
    F: FnMut(RemoteOpenProgressEvent),
{
    emit(
        request.request_id,
        RemoteOpenPhaseKind::Platform,
        RemoteOpenPhaseStatus::Running,
        0,
        Some("正在检测远程平台".into()),
        progress,
    );
    let output = run_remote_command(
        request,
        &platform_detection_command(strategy),
        PLATFORM_PROBE_TIMEOUT,
        runner,
    );
    if !output.success {
        let message = output_error("远程平台检测失败", &output);
        emit(
            request.request_id,
            RemoteOpenPhaseKind::Platform,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }

    let platform = parse_platform_output(&output.stdout);
    if !platform.os.eq_ignore_ascii_case("linux") {
        let message = format!(
            "远程 Agent bootstrap 仅支持 Linux，当前远程系统为 {}",
            platform.os
        );
        emit(
            request.request_id,
            RemoteOpenPhaseKind::Platform,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }
    if !platform.missing_tools.is_empty() {
        let message = format!(
            "远程机器缺少 bootstrap 依赖：{}。请先安装后重试。",
            platform.missing_tools.join(", ")
        );
        emit(
            request.request_id,
            RemoteOpenPhaseKind::Platform,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }
    if platform.home.trim().is_empty() {
        let message = "无法解析远程用户 HOME，不能准备用户级 Agent runtime".to_string();
        emit(
            request.request_id,
            RemoteOpenPhaseKind::Platform,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }

    emit(
        request.request_id,
        RemoteOpenPhaseKind::Platform,
        RemoteOpenPhaseStatus::Succeeded,
        output.elapsed_ms,
        Some(format!("远程平台 {} {}", platform.os, platform.arch)),
        progress,
    );
    Ok(platform)
}

fn validate_remote_path<R, F>(
    request: &RemoteAgentBootstrapRequest<'_>,
    runner: &R,
    progress: &mut F,
) -> Result<()>
where
    R: RemoteSshCommandRunner,
    F: FnMut(RemoteOpenProgressEvent),
{
    emit(
        request.request_id,
        RemoteOpenPhaseKind::RemotePath,
        RemoteOpenPhaseStatus::Running,
        0,
        Some("正在检查远程目录".into()),
        progress,
    );
    let Some(command) = remote_directory_test_command(request.remote_path) else {
        let message = "远程目录必须是绝对路径、~ 或 ~/path".to_string();
        emit(
            request.request_id,
            RemoteOpenPhaseKind::RemotePath,
            RemoteOpenPhaseStatus::Failed,
            0,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    };
    let output = run_remote_command(request, &command, SSH_PROBE_TIMEOUT, runner);
    if !output.success {
        let message = output_error("远程目录不可用", &output);
        emit(
            request.request_id,
            RemoteOpenPhaseKind::RemotePath,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }
    emit(
        request.request_id,
        RemoteOpenPhaseKind::RemotePath,
        RemoteOpenPhaseStatus::Succeeded,
        output.elapsed_ms,
        Some("远程目录可用".into()),
        progress,
    );
    Ok(())
}

fn prepare_runtime_directory<R, F>(
    request: &RemoteAgentBootstrapRequest<'_>,
    strategy: AgentBootstrapStrategy,
    runner: &R,
    progress: &mut F,
) -> Result<String>
where
    R: RemoteSshCommandRunner,
    F: FnMut(RemoteOpenProgressEvent),
{
    emit(
        request.request_id,
        RemoteOpenPhaseKind::RuntimeDirectory,
        RemoteOpenPhaseStatus::Running,
        0,
        Some("正在准备远程 Agent runtime 目录".into()),
        progress,
    );
    let output = run_remote_command(
        request,
        &runtime_directory_command(strategy),
        RUNTIME_PREPARE_TIMEOUT,
        runner,
    );
    if !output.success {
        let message = output_error("远程 runtime 目录准备失败", &output);
        emit(
            request.request_id,
            RemoteOpenPhaseKind::RuntimeDirectory,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }
    let runtime_base = output.stdout.trim().to_string();
    if runtime_base.is_empty() {
        let message = "远程 runtime 目录准备成功但没有返回路径".to_string();
        emit(
            request.request_id,
            RemoteOpenPhaseKind::RuntimeDirectory,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }
    emit(
        request.request_id,
        RemoteOpenPhaseKind::RuntimeDirectory,
        RemoteOpenPhaseStatus::Succeeded,
        output.elapsed_ms,
        Some(format!("runtime: {runtime_base}")),
        progress,
    );
    Ok(runtime_base)
}

fn install_or_reuse_agent<R, F>(
    request: &RemoteAgentBootstrapRequest<'_>,
    strategy: AgentBootstrapStrategy,
    platform: &RemotePlatform,
    runtime_base: &str,
    runner: &R,
    progress: &mut F,
) -> Result<()>
where
    R: RemoteSshCommandRunner,
    F: FnMut(RemoteOpenProgressEvent),
{
    emit(
        request.request_id,
        RemoteOpenPhaseKind::AgentInstall,
        RemoteOpenPhaseStatus::Running,
        0,
        Some(format!("正在准备 {}", strategy.binary)),
        progress,
    );
    let command = match install_or_reuse_command(strategy, platform, runtime_base) {
        Ok(command) => command,
        Err(error) => {
            let message = error.to_string();
            emit(
                request.request_id,
                RemoteOpenPhaseKind::AgentInstall,
                RemoteOpenPhaseStatus::Failed,
                0,
                Some(message.clone()),
                progress,
            );
            return Err(error);
        }
    };
    let output = run_remote_command(request, &command, AGENT_INSTALL_TIMEOUT, runner);
    if !output.success {
        let message = output_error("远程 Agent 安装失败", &output);
        emit(
            request.request_id,
            RemoteOpenPhaseKind::AgentInstall,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }
    let action = output.stdout.trim();
    let message = if action.contains("reused") {
        "已复用远程 Agent runtime"
    } else {
        "已安装远程 Agent runtime"
    };
    emit(
        request.request_id,
        RemoteOpenPhaseKind::AgentInstall,
        RemoteOpenPhaseStatus::Succeeded,
        output.elapsed_ms,
        Some(message.into()),
        progress,
    );
    Ok(())
}

fn verify_agent<R, F>(
    request: &RemoteAgentBootstrapRequest<'_>,
    strategy: AgentBootstrapStrategy,
    runtime_base: &str,
    runner: &R,
    progress: &mut F,
) -> Result<String>
where
    R: RemoteSshCommandRunner,
    F: FnMut(RemoteOpenProgressEvent),
{
    emit(
        request.request_id,
        RemoteOpenPhaseKind::AgentVerify,
        RemoteOpenPhaseStatus::Running,
        0,
        Some("正在验证远程 Agent 可执行文件".into()),
        progress,
    );
    let output = run_remote_command(
        request,
        &verify_agent_command(strategy, runtime_base),
        AGENT_VERIFY_TIMEOUT,
        runner,
    );
    if !output.success {
        let message = output_error("远程 Agent 验证失败", &output);
        emit(
            request.request_id,
            RemoteOpenPhaseKind::AgentVerify,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }
    let binary_path = output.stdout.trim();
    if binary_path.is_empty() {
        let message = "远程 Agent 验证成功但没有返回可执行文件路径".to_string();
        emit(
            request.request_id,
            RemoteOpenPhaseKind::AgentVerify,
            RemoteOpenPhaseStatus::Failed,
            output.elapsed_ms,
            Some(message.clone()),
            progress,
        );
        return Err(anyhow!(message));
    }
    let command = agent_command_for_verified_binary(binary_path, strategy.args);
    emit(
        request.request_id,
        RemoteOpenPhaseKind::AgentVerify,
        RemoteOpenPhaseStatus::Succeeded,
        output.elapsed_ms,
        Some("远程 Agent 已就绪".into()),
        progress,
    );
    Ok(command)
}

fn run_remote_command<R>(
    request: &RemoteAgentBootstrapRequest<'_>,
    remote_command: &str,
    timeout: Duration,
    runner: &R,
) -> crate::remote_ssh::RemoteSshOutput
where
    R: RemoteSshCommandRunner,
{
    runner.run_ssh_command(&RemoteSshCommand::new(
        request.profile.ssh_target.clone(),
        request.profile.ssh_port,
        remote_command.to_string(),
        request.ssh_password,
        timeout,
    ))
}

fn emit<F>(
    request_id: Uuid,
    phase: RemoteOpenPhaseKind,
    status: RemoteOpenPhaseStatus,
    elapsed_ms: u64,
    message: Option<String>,
    progress: &mut F,
) where
    F: FnMut(RemoteOpenProgressEvent),
{
    progress(RemoteOpenProgressEvent {
        request_id,
        phase,
        status,
        elapsed_ms,
        message: message.map(|message| sanitize_ssh_diagnostic(&message)),
    });
}

fn output_error(prefix: &str, output: &crate::remote_ssh::RemoteSshOutput) -> String {
    if output.timed_out {
        return format!("{prefix}：SSH 命令超时");
    }
    let message = first_nonempty(&output.stderr, &output.stdout)
        .map(sanitize_ssh_diagnostic)
        .unwrap_or_else(|| "SSH 命令失败但没有输出".into());
    format!("{prefix}：{message}")
}

fn platform_detection_command(strategy: AgentBootstrapStrategy) -> String {
    let required: Vec<&str> = match strategy.install {
        AgentInstallStrategy::GithubReleaseArchive { .. } => {
            vec!["uname", "mkdir", "rm", "mv", "tar", "chmod"]
        }
        AgentInstallStrategy::NpmPackage { .. } => {
            vec!["uname", "mkdir", "rm", "mv", "node", "npm"]
        }
    };
    let download_probe = match strategy.install {
        AgentInstallStrategy::GithubReleaseArchive { .. } => {
            "if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then printf 'missing=%s\\n' 'curl-or-wget'; fi; "
        }
        AgentInstallStrategy::NpmPackage { .. } => "",
    };
    format!(
        "printf 'os=%s\\n' \"$(uname -s 2>/dev/null || true)\"; \
         printf 'arch=%s\\n' \"$(uname -m 2>/dev/null || true)\"; \
         printf 'home=%s\\n' \"$HOME\"; \
         for kodex_cmd in {}; do command -v \"$kodex_cmd\" >/dev/null 2>&1 || printf 'missing=%s\\n' \"$kodex_cmd\"; done; \
         {download_probe}",
        required.join(" ")
    )
}

pub fn parse_platform_output(output: &str) -> RemotePlatform {
    let mut platform = RemotePlatform {
        os: String::new(),
        arch: String::new(),
        home: String::new(),
        missing_tools: Vec::new(),
    };
    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "os" => platform.os = value.trim().to_string(),
            "arch" => platform.arch = value.trim().to_string(),
            "home" => platform.home = value.trim().to_string(),
            "missing" => platform.missing_tools.push(value.trim().to_string()),
            _ => {}
        }
    }
    platform
}

pub fn remote_directory_test_command(remote_path: &str) -> Option<String> {
    let path = remote_path.trim();
    if path == "~" {
        return Some("test -d \"$HOME\"".into());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if rest.trim().is_empty() {
            return Some("test -d \"$HOME\"".into());
        }
        return Some(format!("test -d \"$HOME\"/{}", shell_quote(rest)));
    }
    path.starts_with('/')
        .then(|| format!("test -d {}", shell_quote(path)))
}

fn runtime_directory_command(strategy: AgentBootstrapStrategy) -> String {
    format!(
        "kodex_base=\"$HOME/{root}/{agent}\"; mkdir -p \"$kodex_base\"; test -d \"$kodex_base\"; printf '%s\\n' \"$kodex_base\"",
        root = REMOTE_RUNTIME_ROOT,
        agent = agent_id(strategy.agent)
    )
}

fn install_or_reuse_command(
    strategy: AgentBootstrapStrategy,
    platform: &RemotePlatform,
    runtime_base: &str,
) -> Result<String> {
    match strategy.install {
        AgentInstallStrategy::GithubReleaseArchive {
            repo,
            url_env,
            linux_x64_asset_glob,
            linux_arm64_asset_glob,
        } => {
            let asset_glob = linux_archive_asset_glob(
                &platform.arch,
                linux_x64_asset_glob,
                linux_arm64_asset_glob,
            )?;
            Ok(github_archive_install_command(
                strategy,
                runtime_base,
                repo,
                url_env,
                asset_glob,
            ))
        }
        AgentInstallStrategy::NpmPackage { package } => Ok(npm_install_or_reuse_command(
            strategy,
            runtime_base,
            package,
        )),
    }
}

fn github_archive_install_command(
    strategy: AgentBootstrapStrategy,
    runtime_base: &str,
    repo: &str,
    url_env: &str,
    asset_glob: &str,
) -> String {
    let base = shell_quote(runtime_base);
    let repo = shell_quote(repo);
    let url_env = shell_quote(url_env);
    let asset_glob = shell_quote(asset_glob);
    format!(
        "kodex_base={base}; \
         kodex_current=\"$kodex_base/current\"; \
         kodex_binary=\"$kodex_current/bin/{binary_name}\"; \
         if [ -x \"$kodex_binary\" ]; then printf 'reused\\n'; \
         else \
           kodex_repo={repo}; \
           kodex_url_env={url_env}; \
           kodex_asset_glob={asset_glob}; \
           eval \"kodex_archive_url=\\${{$kodex_url_env:-}}\"; \
           kodex_fetch() {{ \
             if command -v curl >/dev/null 2>&1; then curl -fsSL \"$1\"; \
             elif command -v wget >/dev/null 2>&1; then wget -qO- \"$1\"; \
             else echo 'missing curl or wget for remote Agent download' >&2; return 127; fi; \
           }}; \
           kodex_download() {{ \
             if command -v curl >/dev/null 2>&1; then curl -fL --connect-timeout 20 --retry 2 -o \"$2\" \"$1\"; \
             elif command -v wget >/dev/null 2>&1; then wget -q -O \"$2\" \"$1\"; \
             else echo 'missing curl or wget for remote Agent download' >&2; return 127; fi; \
           }}; \
           if [ -z \"$kodex_archive_url\" ]; then \
             kodex_release_api=\"https://api.github.com/repos/$kodex_repo/releases/latest\"; \
             kodex_archive_url=\"$(kodex_fetch \"$kodex_release_api\" | while IFS= read -r kodex_line; do \
               case \"$kodex_line\" in \
                 *browser_download_url*$kodex_asset_glob*) \
                   kodex_url=${{kodex_line#*browser_download_url}}; \
                   kodex_url=${{kodex_url#*\\\"}}; \
                   kodex_url=${{kodex_url#*\\\"}}; \
                   kodex_url=${{kodex_url%%\\\"*}}; \
                   printf '%s\\n' \"$kodex_url\"; \
                   break; \
                 ;; \
               esac; \
             done)\"; \
           fi; \
           if [ -z \"$kodex_archive_url\" ]; then echo \"no release asset matched $kodex_asset_glob in $kodex_repo\" >&2; exit 1; fi; \
           kodex_tmp=\"$kodex_base/.install-$$\"; \
           rm -rf \"$kodex_tmp\"; \
           mkdir -p \"$kodex_tmp/bin\"; \
           kodex_archive=\"$kodex_tmp/agent.tar.gz\"; \
           kodex_download \"$kodex_archive_url\" \"$kodex_archive\"; \
           tar -xzf \"$kodex_archive\" -C \"$kodex_tmp\"; \
           rm -f \"$kodex_archive\"; \
           if [ -x \"$kodex_tmp/{binary_name}\" ]; then mv \"$kodex_tmp/{binary_name}\" \"$kodex_tmp/bin/{binary_name}\"; fi; \
           test -x \"$kodex_tmp/bin/{binary_name}\"; \
           chmod +x \"$kodex_tmp/bin/{binary_name}\"; \
           rm -rf \"$kodex_base/current.previous\"; \
           if [ -e \"$kodex_current\" ] || [ -L \"$kodex_current\" ]; then mv \"$kodex_current\" \"$kodex_base/current.previous\"; fi; \
           mv \"$kodex_tmp\" \"$kodex_current\"; \
           rm -rf \"$kodex_base/current.previous\"; \
           printf 'installed\\n'; \
         fi",
        binary_name = strategy.binary
    )
}

fn npm_install_or_reuse_command(
    strategy: AgentBootstrapStrategy,
    runtime_base: &str,
    package: &str,
) -> String {
    let base = shell_quote(runtime_base);
    let package = shell_quote(package);
    format!(
        "kodex_base={base}; \
         kodex_current=\"$kodex_base/current\"; \
         kodex_binary=\"$kodex_current/bin/{binary_name}\"; \
         if [ -x \"$kodex_binary\" ]; then printf 'reused\\n'; \
         else \
           if ! command -v npm >/dev/null 2>&1; then echo 'missing npm for remote Agent install' >&2; exit 127; fi; \
           kodex_tmp=\"$kodex_base/.install-$$\"; \
           rm -rf \"$kodex_tmp\"; \
           mkdir -p \"$kodex_tmp\"; \
           npm install --global --prefix \"$kodex_tmp\" {package}; \
           test -x \"$kodex_tmp/bin/{binary_name}\"; \
           rm -rf \"$kodex_base/current.previous\"; \
           if [ -e \"$kodex_current\" ] || [ -L \"$kodex_current\" ]; then mv \"$kodex_current\" \"$kodex_base/current.previous\"; fi; \
           mv \"$kodex_tmp\" \"$kodex_current\"; \
           rm -rf \"$kodex_base/current.previous\"; \
           printf 'installed\\n'; \
         fi",
        binary_name = strategy.binary
    )
}

fn linux_archive_asset_glob<'a>(
    arch: &str,
    linux_x64_asset_glob: &'a str,
    linux_arm64_asset_glob: &'a str,
) -> Result<&'a str> {
    match arch.trim().to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" => Ok(linux_x64_asset_glob),
        "aarch64" | "arm64" => Ok(linux_arm64_asset_glob),
        other => Err(anyhow!(
            "远程 Agent bootstrap 不支持当前 Linux CPU 架构：{}",
            other
        )),
    }
}

fn verify_agent_command(strategy: AgentBootstrapStrategy, runtime_base: &str) -> String {
    let base = shell_quote(runtime_base);
    format!(
        "kodex_binary={base}/current/bin/{binary}; test -x \"$kodex_binary\"; printf '%s\\n' \"$kodex_binary\"",
        binary = strategy.binary
    )
}

fn agent_command_for_verified_binary(binary_path: &str, args: &[&str]) -> String {
    let mut parts = vec![shell_quote(binary_path)];
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn agent_id(agent: AgentCliId) -> &'static str {
    match agent {
        AgentCliId::CodexAcp => "codex-acp",
        AgentCliId::ClaudeAgentAcp => "claude-agent-acp",
        AgentCliId::Codebuddy => "codebuddy",
        AgentCliId::Goose => "goose",
    }
}

fn shell_quote(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_ssh::{RemoteSshOutput, build_ssh_args};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeRunner {
        outputs: Arc<Mutex<Vec<RemoteSshOutput>>>,
        commands: Arc<Mutex<Vec<RemoteSshCommand>>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<RemoteSshOutput>) -> Self {
            Self {
                outputs: Arc::new(Mutex::new(outputs)),
                commands: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn commands(&self) -> Vec<RemoteSshCommand> {
            self.commands.lock().unwrap().clone()
        }
    }

    impl RemoteSshCommandRunner for FakeRunner {
        fn run_ssh_command(&self, command: &RemoteSshCommand) -> RemoteSshOutput {
            self.commands.lock().unwrap().push(command.clone());
            self.outputs.lock().unwrap().remove(0)
        }
    }

    fn output(stdout: &str) -> RemoteSshOutput {
        RemoteSshOutput {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
            timed_out: false,
            elapsed_ms: 7,
        }
    }

    fn failed(stderr: &str) -> RemoteSshOutput {
        RemoteSshOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.into(),
            timed_out: false,
            elapsed_ms: 7,
        }
    }

    fn profile() -> RemoteMachineProfile {
        RemoteMachineProfile {
            id: Uuid::new_v4(),
            display_name: "Devbox".into(),
            ssh_target: "root@devbox".into(),
            ssh_port: Some(36000),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_validation: None,
        }
    }

    fn request<'a>(id: Uuid, profile: &'a RemoteMachineProfile) -> RemoteAgentBootstrapRequest<'a> {
        RemoteAgentBootstrapRequest {
            request_id: id,
            profile,
            remote_path: "/srv/project",
            ssh_password: Some("ssh-secret"),
            agent_cli: AgentCliId::CodexAcp,
        }
    }

    #[test]
    fn parses_platform_output() {
        let platform = parse_platform_output("os=Linux\narch=x86_64\nhome=/root\nmissing=npm\n");

        assert_eq!(platform.os, "Linux");
        assert_eq!(platform.arch, "x86_64");
        assert_eq!(platform.home, "/root");
        assert_eq!(platform.missing_tools, vec!["npm"]);
    }

    #[test]
    fn bootstrap_reuses_existing_runtime_and_returns_verified_command() {
        let id = Uuid::new_v4();
        let profile = profile();
        let runner = FakeRunner::new(vec![
            output("kodex-ssh-ok"),
            output("os=Linux\narch=x86_64\nhome=/root\n"),
            output(""),
            output("/root/.kodex/remote-agents/codex-acp\n"),
            output("reused\n"),
            output("/root/.kodex/remote-agents/codex-acp/current/bin/codex-acp\n"),
        ]);
        let mut events = Vec::new();

        let result =
            bootstrap_remote_agent(request(id, &profile), &runner, |event| events.push(event))
                .unwrap();

        assert_eq!(
            result.agent_command,
            "/root/.kodex/remote-agents/codex-acp/current/bin/codex-acp"
        );
        assert_eq!(events.first().unwrap().phase, RemoteOpenPhaseKind::Ssh);
        assert!(
            events
                .iter()
                .any(|event| event.phase == RemoteOpenPhaseKind::AgentInstall
                    && event.status == RemoteOpenPhaseStatus::Succeeded)
        );
        let commands = runner.commands();
        assert_eq!(commands.len(), 6);
        assert!(
            commands[1]
                .remote_command
                .contains("missing=%s\\n' 'curl-or-wget'")
        );
        assert!(!commands[1].remote_command.contains("node"));
        assert!(commands[4].remote_command.contains("kodex_repo=koth/Kodex"));
        assert!(
            commands[4]
                .remote_command
                .contains("api.github.com/repos/$kodex_repo/releases/latest")
        );
        assert!(
            commands[4]
                .remote_command
                .contains("codex-acp-linux-x64.tar.gz")
        );
        assert!(!commands[4].remote_command.contains("npm install --global"));
        assert!(build_ssh_args(&commands[0]).contains(&"NumberOfPasswordPrompts=1".to_string()));
    }

    #[test]
    fn codebuddy_bootstrap_fails_when_required_node_tools_are_missing() {
        let id = Uuid::new_v4();
        let profile = profile();
        let runner = FakeRunner::new(vec![
            output("kodex-ssh-ok"),
            output("os=Linux\narch=x86_64\nhome=/root\nmissing=node\n"),
        ]);
        let mut request = request(id, &profile);
        request.agent_cli = AgentCliId::Codebuddy;
        let mut events = Vec::new();

        let error =
            bootstrap_remote_agent(request, &runner, |event| events.push(event)).unwrap_err();

        assert!(error.to_string().contains("缺少 bootstrap 依赖"));
        assert_eq!(events.last().unwrap().status, RemoteOpenPhaseStatus::Failed);
        assert!(runner.commands()[1].remote_command.contains("node"));
        assert!(runner.commands()[1].remote_command.contains("npm"));
    }

    #[test]
    fn claude_bootstrap_downloads_platform_binary_archive() {
        let id = Uuid::new_v4();
        let profile = profile();
        let runner = FakeRunner::new(vec![
            output("kodex-ssh-ok"),
            output("os=Linux\narch=aarch64\nhome=/root\n"),
            output(""),
            output("/root/.kodex/remote-agents/claude-agent-acp\n"),
            output("installed\n"),
            output("/root/.kodex/remote-agents/claude-agent-acp/current/bin/claude-agent-acp\n"),
        ]);
        let mut request = request(id, &profile);
        request.agent_cli = AgentCliId::ClaudeAgentAcp;
        let mut events = Vec::new();

        let result = bootstrap_remote_agent(request, &runner, |event| events.push(event)).unwrap();

        assert_eq!(
            result.agent_command,
            "/root/.kodex/remote-agents/claude-agent-acp/current/bin/claude-agent-acp"
        );
        let commands = runner.commands();
        assert!(commands[4].remote_command.contains("kodex_repo=koth/Kodex"));
        assert!(
            commands[4]
                .remote_command
                .contains("kodex-claude-linux-arm64.tar.gz")
        );
        assert!(
            commands[4]
                .remote_command
                .contains("KODEX_REMOTE_CLAUDE_AGENT_ACP_ARCHIVE_URL")
        );
    }

    #[test]
    fn bootstrap_fails_for_unsupported_remote_platform() {
        let id = Uuid::new_v4();
        let profile = profile();
        let runner = FakeRunner::new(vec![
            output("kodex-ssh-ok"),
            output("os=Darwin\narch=arm64\nhome=/Users/dev\n"),
        ]);
        let mut events = Vec::new();

        let error =
            bootstrap_remote_agent(request(id, &profile), &runner, |event| events.push(event))
                .unwrap_err();

        assert!(error.to_string().contains("仅支持 Linux"));
        assert!(
            events
                .iter()
                .any(|event| event.phase == RemoteOpenPhaseKind::Platform
                    && event.status == RemoteOpenPhaseStatus::Failed)
        );
    }

    #[test]
    fn bootstrap_fails_for_unsupported_agent_before_ssh() {
        let id = Uuid::new_v4();
        let profile = profile();
        let runner = FakeRunner::new(Vec::new());
        let mut request = request(id, &profile);
        request.agent_cli = AgentCliId::Goose;
        let mut events = Vec::new();

        let error =
            bootstrap_remote_agent(request, &runner, |event| events.push(event)).unwrap_err();

        assert!(error.to_string().contains("not supported"));
        assert!(events.is_empty());
        assert!(runner.commands().is_empty());
    }

    #[test]
    fn bootstrap_sanitizes_secret_diagnostics() {
        let id = Uuid::new_v4();
        let profile = profile();
        let runner = FakeRunner::new(vec![failed("password=ssh-secret")]);
        let mut events = Vec::new();

        let error =
            bootstrap_remote_agent(request(id, &profile), &runner, |event| events.push(event))
                .unwrap_err();

        assert!(!error.to_string().contains("ssh-secret"));
        assert!(!format!("{events:?}").contains("ssh-secret"));
    }

    #[test]
    fn codebuddy_strategy_adds_acp_arg_to_verified_command() {
        let command = agent_command_for_verified_binary(
            "/home/user/.kodex/remote-agents/codebuddy/current/bin/codebuddy",
            strategy(AgentCliId::Codebuddy).unwrap().args,
        );

        assert_eq!(
            command,
            "/home/user/.kodex/remote-agents/codebuddy/current/bin/codebuddy --acp --acp-transport streamable-http"
        );
    }
}
