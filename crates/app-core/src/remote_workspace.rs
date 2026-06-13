use crate::remote_ssh::{
    RemoteSshCommand, RemoteSshCommandRunner, SystemRemoteSshCommandRunner, first_nonempty,
    sanitize_ssh_diagnostic,
};
use acp_core::RemoteSshSessionConfig;
use anyhow::{Context, anyhow, bail};
use serde::Deserialize;
use similar::{ChangeTag, TextDiff};
use std::path::PathBuf;
use std::time::Duration;
use workspace_model::{
    ChangeSection, ChangedFile, DiffHunk, DiffLine, DiffLineKind, DiffStats, EditorFileSnapshot,
    FileChangeType, FileEntry, PatchStatus, RepositorySnapshot, SearchResult, SessionFileChange,
};

const REMOTE_LIST_DIR_TIMEOUT: Duration = Duration::from_secs(12);
const REMOTE_READ_FILE_TIMEOUT: Duration = Duration::from_secs(45);
const REMOTE_SAVE_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_GIT_TIMEOUT: Duration = Duration::from_secs(20);
const REMOTE_SEARCH_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) struct RemoteWorkspaceClient<'a, R = SystemRemoteSshCommandRunner> {
    config: &'a RemoteSshSessionConfig,
    runner: R,
}

impl<'a> RemoteWorkspaceClient<'a> {
    pub(crate) fn new(config: &'a RemoteSshSessionConfig) -> Self {
        Self {
            config,
            runner: SystemRemoteSshCommandRunner,
        }
    }
}

impl<'a, R> RemoteWorkspaceClient<'a, R>
where
    R: RemoteSshCommandRunner,
{
    #[cfg(test)]
    pub(crate) fn with_runner(config: &'a RemoteSshSessionConfig, runner: R) -> Self {
        Self { config, runner }
    }

    pub(crate) fn list_dir(&self, path: &str) -> anyhow::Result<Vec<FileEntry>> {
        let path = sanitize_relative_path(path, true)?;
        let response: RemoteListResponse =
            self.run_node_json(LIST_DIR_SCRIPT, &[path.as_str()], None, REMOTE_LIST_DIR_TIMEOUT)?;
        Ok(response
            .entries
            .into_iter()
            .map(|entry| FileEntry {
                name: entry.name,
                kind: entry.kind,
                path: entry.path,
            })
            .collect())
    }

    pub(crate) fn read_file(&self, path: &str) -> anyhow::Result<EditorFileSnapshot> {
        let path = sanitize_relative_path(path, false)?;
        self.run_node_json(
            READ_FILE_SCRIPT,
            &[path.as_str()],
            None,
            REMOTE_READ_FILE_TIMEOUT,
        )
    }

    pub(crate) fn save_file(
        &self,
        path: &str,
        content: &str,
        base_version_hash: Option<&str>,
        base_version_size: Option<u64>,
        overwrite: bool,
    ) -> anyhow::Result<EditorFileSnapshot> {
        let path = sanitize_relative_path(path, false)?;
        let base_hash = base_version_hash.unwrap_or("");
        let base_size = base_version_size
            .map(|size| size.to_string())
            .unwrap_or_default();
        let overwrite = if overwrite { "1" } else { "0" };
        self.run_node_json(
            SAVE_FILE_SCRIPT,
            &[path.as_str(), base_hash, &base_size, overwrite],
            Some(content.as_bytes().to_vec()),
            REMOTE_SAVE_TIMEOUT,
        )
    }

    pub(crate) fn rename(&self, path: &str, new_name: &str) -> anyhow::Result<FileEntry> {
        let path = sanitize_relative_path(path, false)?;
        let new_name = validate_new_name(new_name)?;
        let entry: RemoteFileEntry = self.run_node_json(
            RENAME_SCRIPT,
            &[path.as_str(), new_name],
            None,
            REMOTE_LIST_DIR_TIMEOUT,
        )?;
        Ok(FileEntry {
            name: entry.name,
            kind: entry.kind,
            path: entry.path,
        })
    }

    pub(crate) fn delete_file(&self, path: &str) -> anyhow::Result<()> {
        let path = sanitize_relative_path(path, false)?;
        let _: RemoteOkResponse = self.run_node_json(
            DELETE_FILE_SCRIPT,
            &[path.as_str()],
            None,
            REMOTE_LIST_DIR_TIMEOUT,
        )?;
        Ok(())
    }

    pub(crate) fn git_status(&self) -> anyhow::Result<RepositorySnapshot> {
        let response: RemoteGitStatusResponse =
            self.run_node_json(GIT_STATUS_SCRIPT, &[], None, REMOTE_GIT_TIMEOUT)?;
        Ok(response.into())
    }

    pub(crate) fn git_stage(&self, paths: &[String]) -> anyhow::Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = Vec::with_capacity(paths.len());
        for path in paths {
            args.push(sanitize_relative_path(path, false)?);
        }
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let _: RemoteOkResponse =
            self.run_node_json(GIT_STAGE_SCRIPT, &arg_refs, None, REMOTE_GIT_TIMEOUT)?;
        Ok(())
    }

    pub(crate) fn git_unstage(&self, paths: &[String]) -> anyhow::Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = Vec::with_capacity(paths.len());
        for path in paths {
            args.push(sanitize_relative_path(path, false)?);
        }
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let _: RemoteOkResponse =
            self.run_node_json(GIT_UNSTAGE_SCRIPT, &arg_refs, None, REMOTE_GIT_TIMEOUT)?;
        Ok(())
    }

    pub(crate) fn git_commit(&self, message: &str) -> anyhow::Result<()> {
        let message = message.trim();
        if message.is_empty() {
            bail!("Commit message cannot be empty");
        }
        let _: RemoteOkResponse = self.run_node_json(
            GIT_COMMIT_SCRIPT,
            &[],
            Some(format!("{message}\n").into_bytes()),
            REMOTE_GIT_TIMEOUT,
        )?;
        Ok(())
    }

    pub(crate) fn search(&self, query: &str) -> anyhow::Result<SearchResult> {
        let query = query.trim();
        if query.is_empty() {
            bail!("Search query must not be empty");
        }
        self.run_node_json(SEARCH_SCRIPT, &[query], None, REMOTE_SEARCH_TIMEOUT)
    }

    pub(crate) fn git_file_diff_auto(
        &self,
        path: &str,
    ) -> anyhow::Result<Option<SessionFileChange>> {
        let path = sanitize_relative_path(path, false)?;
        for section in ["staged", "unstaged", "untracked"] {
            if let Some(change) = self.git_file_diff_for_section(&path, section)? {
                return Ok(Some(change));
            }
        }
        Ok(None)
    }

    pub(crate) fn git_file_diff(
        &self,
        path: &str,
        section: ChangeSection,
    ) -> anyhow::Result<Option<SessionFileChange>> {
        let path = sanitize_relative_path(path, false)?;
        self.git_file_diff_for_section(&path, section_arg(&section))
    }

    fn git_file_diff_for_section(
        &self,
        path: &str,
        section: &str,
    ) -> anyhow::Result<Option<SessionFileChange>> {
        let response: RemoteGitDiffResponse =
            self.run_node_json(GIT_DIFF_SCRIPT, &[path, section], None, REMOTE_GIT_TIMEOUT)?;
        let Some(record) = response.record else {
            return Ok(None);
        };
        let (added, removed) = diff_stats(record.old_text.as_deref(), record.new_text.as_deref());
        Ok(Some(SessionFileChange {
            path: record.path,
            change_type: record.change_type,
            old_text: record.old_text,
            new_text: record.new_text.unwrap_or_default(),
            added_lines: added,
            removed_lines: removed,
            timestamp: current_timestamp(),
        }))
    }

    fn run_node_json<T: for<'de> Deserialize<'de>>(
        &self,
        script: &str,
        args: &[&str],
        stdin: Option<Vec<u8>>,
        timeout: Duration,
    ) -> anyhow::Result<T> {
        let script = with_common_script(script);
        let command = node_command(&script, &self.config.remote_workspace_root, args);
        let mut request = RemoteSshCommand::new(
            self.config.ssh_target.clone(),
            self.config.ssh_port,
            command,
            self.config.ssh_password.as_deref(),
            timeout,
        );
        if let Some(stdin) = stdin {
            request = request.with_stdin(stdin);
        }
        let output = self.runner.run_ssh_command(&request);
        if !output.success {
            return Err(anyhow!(remote_error("远程命令失败", &output)));
        }
        serde_json::from_str(&output.stdout)
            .with_context(|| format!("远程响应不是有效 JSON：{}", output.stdout.trim()))
    }
}

fn with_common_script(body: &str) -> String {
    let mut script = String::with_capacity(COMMON_SCRIPT.len() + body.len());
    script.push_str(COMMON_SCRIPT);
    script.push_str(body);
    script
}

fn node_command(script: &str, remote_root: &str, args: &[&str]) -> String {
    let mut parts = vec![
        "node".to_string(),
        "-e".to_string(),
        shell_quote(script),
        "--".to_string(),
        shell_quote(remote_root),
    ];
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn remote_error(prefix: &str, output: &crate::remote_ssh::RemoteSshOutput) -> String {
    if output.timed_out {
        return format!("{prefix}：SSH 命令超时");
    }
    let message = first_nonempty(&output.stderr, &output.stdout)
        .map(sanitize_ssh_diagnostic)
        .unwrap_or_else(|| "SSH 命令失败但没有输出".into());
    format!("{prefix}：{message}")
}

fn shell_quote(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

fn sanitize_relative_path(path: &str, allow_empty: bool) -> anyhow::Result<String> {
    let normalized = path.trim().replace('\\', "/");
    let normalized = normalized.trim_start_matches("./").trim_end_matches('/');
    if normalized.is_empty() {
        if allow_empty {
            return Ok(String::new());
        }
        bail!("路径不能为空");
    }
    if normalized.starts_with('/') {
        bail!("远程工作区路径必须使用相对路径");
    }
    if normalized
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("不允许路径遍历");
    }
    Ok(normalized.to_string())
}

fn validate_new_name(name: &str) -> anyhow::Result<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("Name cannot be empty");
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains('/') || trimmed.contains('\\') {
        bail!("Name must be a single file or folder name");
    }
    Ok(trimmed)
}

fn section_arg(section: &ChangeSection) -> &'static str {
    match section {
        ChangeSection::Staged => "staged",
        ChangeSection::Unstaged => "unstaged",
        ChangeSection::Untracked => "untracked",
    }
}

fn diff_stats(old_text: Option<&str>, new_text: Option<&str>) -> (usize, usize) {
    let old = normalize_line_endings(old_text.unwrap_or_default());
    let new = normalize_line_endings(new_text.unwrap_or_default());
    let diff = TextDiff::from_lines(&old, &new);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {}
            ChangeTag::Delete => removed += 1,
            ChangeTag::Insert => added += 1,
        }
    }
    (added, removed)
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn current_timestamp() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

#[derive(Debug, Deserialize)]
struct RemoteOkResponse {
    #[allow(dead_code)]
    ok: bool,
}

#[derive(Debug, Deserialize)]
struct RemoteListResponse {
    entries: Vec<RemoteFileEntry>,
}

#[derive(Debug, Deserialize)]
struct RemoteFileEntry {
    name: String,
    kind: workspace_model::FileEntryKind,
    path: String,
}

#[derive(Debug, Deserialize)]
struct RemoteGitDiffResponse {
    record: Option<RemoteGitDiffRecord>,
}

#[derive(Debug, Deserialize)]
struct RemoteGitDiffRecord {
    path: String,
    change_type: FileChangeType,
    old_text: Option<String>,
    new_text: Option<String>,
}

fn changed_file(path: String, section: ChangeSection, added: usize, removed: usize) -> ChangedFile {
    ChangedFile {
        path: PathBuf::from(path),
        section,
        stats: DiffStats { added, removed },
        patch_status: PatchStatus::Proposed,
        hunks: vec![DiffHunk {
            heading: "远程工作树变更".into(),
            lines: vec![DiffLine {
                kind: DiffLineKind::Context,
                content: "Remote Git diff".into(),
            }],
        }],
    }
}

#[derive(Debug, Deserialize)]
struct RemoteGitStatusResponse {
    branch: String,
    head: String,
    changed_files: Vec<RemoteChangedFile>,
}

#[derive(Debug, Deserialize)]
struct RemoteChangedFile {
    path: String,
    section: ChangeSection,
    added: usize,
    removed: usize,
}

impl From<RemoteGitStatusResponse> for RepositorySnapshot {
    fn from(value: RemoteGitStatusResponse) -> Self {
        RepositorySnapshot {
            branch: value.branch,
            head: value.head,
            changed_files: value
                .changed_files
                .into_iter()
                .map(|file| changed_file(file.path, file.section, file.added, file.removed))
                .collect(),
        }
    }
}

const COMMON_SCRIPT: &str = r#"
const fs = require('fs');
const path = require('path');
const cp = require('child_process');
const { TextDecoder } = require('util');

function die(message) {
  console.error(message);
  process.exit(1);
}

function rootAndRel() {
  const rootArg = process.argv[1];
  const relArg = process.argv[2] || '';
  if (!rootArg || !path.isAbsolute(rootArg)) die('remote root must be absolute');
  if (path.isAbsolute(relArg) || relArg.split(/[\\/]+/).includes('..')) die('path escapes workspace');
  const root = fs.realpathSync(rootArg);
  const target = path.resolve(root, relArg);
  return { root, rel: relArg, target };
}

function ensureInside(root, target) {
  const normalizedRoot = root.endsWith(path.sep) ? root : root + path.sep;
  if (target !== root && !target.startsWith(normalizedRoot)) die('path escapes workspace');
}

function relPath(root, target) {
  return path.relative(root, target).split(path.sep).join('/');
}

function fnv1a64(bytes) {
  let hash = 0xcbf29ce484222325n;
  const prime = 0x100000001b3n;
  const mask = 0xffffffffffffffffn;
  for (const byte of bytes) {
    hash ^= BigInt(byte);
    hash = (hash * prime) & mask;
  }
  return hash.toString(16).padStart(16, '0');
}

function mimeType(file) {
  switch (path.extname(file).toLowerCase()) {
    case '.png': return 'image/png';
    case '.jpg':
    case '.jpeg': return 'image/jpeg';
    case '.gif': return 'image/gif';
    case '.webp': return 'image/webp';
    case '.bmp': return 'image/bmp';
    case '.svg': return 'image/svg+xml';
    case '.ico': return 'image/x-icon';
    default: return null;
  }
}

function snapshot(root, target) {
  const real = fs.realpathSync(target);
  ensureInside(root, real);
  const stat = fs.statSync(real);
  if (!stat.isFile()) die('not a file');
  if (stat.size > 5 * 1024 * 1024) die('file is too large for safe editing');
  const bytes = fs.readFileSync(real);
  const mime = mimeType(real);
  let content;
  let kind = 'text';
  if (mime) {
    kind = 'image';
    content = `data:${mime};base64,${bytes.toString('base64')}`;
  } else {
    if (bytes.includes(0)) die('binary file cannot be edited safely');
    try {
      content = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    } catch (_) {
      die('non-UTF-8 file cannot be edited safely');
    }
  }
  return {
    path: relPath(root, real),
    content,
    version: {
      content_hash: fnv1a64(bytes),
      modified_ms: Math.floor(stat.mtimeMs),
      size: stat.size
    },
    kind,
    mime_type: mime
  };
}

function git(args, options = {}) {
  try {
    return cp.execFileSync('git', args, {
      cwd: options.cwd,
      encoding: options.encoding || 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      maxBuffer: 16 * 1024 * 1024
    });
  } catch (error) {
    const stderr = error.stderr ? String(error.stderr) : String(error.message || error);
    die(stderr.trim() || 'git command failed');
  }
}

function gitMaybe(args, options = {}) {
  try {
    return cp.execFileSync('git', args, {
      cwd: options.cwd,
      encoding: options.encoding || 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      maxBuffer: 16 * 1024 * 1024
    });
  } catch (_) {
    return null;
  }
}

function lineCount(text) {
  if (!text) return 0;
  return text.endsWith('\n') ? text.split('\n').length - 1 : text.split('\n').length;
}

function textOrNullFromBuffer(buffer) {
  if (buffer == null) return null;
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(Buffer.from(buffer));
  } catch (_) {
    return null;
  }
}
"#;

const LIST_DIR_SCRIPT: &str = r#"
const { root, rel, target } = rootAndRel();
const real = fs.realpathSync(target);
ensureInside(root, real);
if (!fs.statSync(real).isDirectory()) die('not a directory');
const entries = fs.readdirSync(real, { withFileTypes: true })
  .filter((entry) => entry.isDirectory() || entry.isFile())
  .map((entry) => {
    const child = path.join(real, entry.name);
    return {
      name: entry.name,
      kind: entry.isDirectory() ? 'Directory' : 'File',
      path: relPath(root, child)
    };
  });
entries.sort((a, b) => {
  if (a.kind !== b.kind) return a.kind === 'Directory' ? -1 : 1;
  return a.name.localeCompare(b.name, undefined, { sensitivity: 'base' });
});
console.log(JSON.stringify({ entries }));
"#;

const READ_FILE_SCRIPT: &str = r#"
const { root, target } = rootAndRel();
console.log(JSON.stringify(snapshot(root, target)));
"#;

const SAVE_FILE_SCRIPT: &str = r#"
const { root, target } = rootAndRel();
const baseHash = process.argv[3] || '';
const baseSize = process.argv[4] || '';
const overwrite = process.argv[5] === '1';
const parent = fs.realpathSync(path.dirname(target));
ensureInside(root, parent);
if (baseHash && !overwrite) {
  if (!fs.existsSync(target)) die('file missing on disk');
  const current = fs.readFileSync(target);
  const currentHash = fnv1a64(current);
  if (currentHash !== baseHash || (baseSize && String(current.length) !== baseSize)) {
    die('file changed on disk');
  }
}
const chunks = [];
process.stdin.on('data', (chunk) => chunks.push(chunk));
process.stdin.on('end', () => {
  fs.writeFileSync(target, Buffer.concat(chunks));
  console.log(JSON.stringify(snapshot(root, target)));
});
"#;

const RENAME_SCRIPT: &str = r#"
const { root, target } = rootAndRel();
const newName = process.argv[3] || '';
if (!newName || newName === '.' || newName === '..' || /[\\/]/.test(newName)) die('invalid new name');
const real = fs.realpathSync(target);
ensureInside(root, real);
const next = path.join(path.dirname(real), newName);
ensureInside(root, path.resolve(next));
if (fs.existsSync(next)) die('target already exists');
fs.renameSync(real, next);
const renamed = fs.realpathSync(next);
const stat = fs.statSync(renamed);
console.log(JSON.stringify({
  name: path.basename(renamed),
  kind: stat.isDirectory() ? 'Directory' : 'File',
  path: relPath(root, renamed)
}));
"#;

const DELETE_FILE_SCRIPT: &str = r#"
const { root, target } = rootAndRel();
const real = fs.realpathSync(target);
ensureInside(root, real);
if (!fs.statSync(real).isFile()) die('cannot delete directories from the file tree');
fs.unlinkSync(real);
console.log(JSON.stringify({ ok: true }));
"#;

const GIT_STATUS_SCRIPT: &str = r#"
const root = fs.realpathSync(process.argv[1]);
const branchRaw = gitMaybe(['rev-parse', '--abbrev-ref', 'HEAD'], { cwd: root });
const headRaw = gitMaybe(['rev-parse', 'HEAD'], { cwd: root });
const branch = (branchRaw || '分离头指针').trim() || '分离头指针';
const head = (headRaw || '未诞生').trim() || '未诞生';
const raw = git(['status', '--porcelain=v1', '-z', '--untracked-files=all'], { cwd: root, encoding: 'buffer' }).toString('utf8');
const parts = raw.split('\0');
const changed_files = [];
function changeType(code) {
  if (code === 'A' || code === '?') return 'Created';
  if (code === 'D') return 'Deleted';
  return 'Modified';
}
function statsFor(section, file) {
  if (section === 'Untracked') {
    const target = path.join(root, file);
    try {
      const text = fs.readFileSync(target, 'utf8');
      return { added: lineCount(text), removed: 0 };
    } catch (_) {
      return { added: 0, removed: 0 };
    }
  }
  const args = section === 'Staged'
    ? ['diff', '--cached', '--numstat', '--', file]
    : ['diff', '--numstat', '--', file];
  const out = gitMaybe(args, { cwd: root }) || '';
  const line = out.split(/\r?\n/).find(Boolean);
  if (!line) return { added: 0, removed: 0 };
  const cols = line.split(/\t/);
  return {
    added: Number.parseInt(cols[0], 10) || 0,
    removed: Number.parseInt(cols[1], 10) || 0
  };
}
for (let i = 0; i < parts.length; i += 1) {
  const rec = parts[i];
  if (!rec) continue;
  const x = rec[0];
  const y = rec[1];
  let file = rec.slice(3);
  if ((x === 'R' || x === 'C') && i + 1 < parts.length) {
    i += 1;
  }
  if (x === '?' && y === '?') {
    const stats = statsFor('Untracked', file);
    changed_files.push({ path: file, section: 'Untracked', ...stats });
    continue;
  }
  if (x && x !== ' ') {
    const stats = statsFor('Staged', file);
    changed_files.push({ path: file, section: 'Staged', ...stats });
  }
  if (y && y !== ' ') {
    const stats = statsFor('Unstaged', file);
    changed_files.push({ path: file, section: 'Unstaged', ...stats });
  }
}
console.log(JSON.stringify({ branch, head, changed_files }));
"#;

const GIT_STAGE_SCRIPT: &str = r#"
const root = fs.realpathSync(process.argv[1]);
const paths = process.argv.slice(2);
if (paths.length === 0) {
  console.log(JSON.stringify({ ok: true }));
  process.exit(0);
}
git(['add', '--', ...paths], { cwd: root });
console.log(JSON.stringify({ ok: true }));
"#;

const GIT_UNSTAGE_SCRIPT: &str = r#"
const root = fs.realpathSync(process.argv[1]);
const paths = process.argv.slice(2);
if (paths.length === 0) {
  console.log(JSON.stringify({ ok: true }));
  process.exit(0);
}
git(['reset', '--', ...paths], { cwd: root });
console.log(JSON.stringify({ ok: true }));
"#;

const GIT_COMMIT_SCRIPT: &str = r#"
const root = fs.realpathSync(process.argv[1]);
const chunks = [];
process.stdin.on('data', (chunk) => chunks.push(chunk));
process.stdin.on('end', () => {
  const message = Buffer.concat(chunks).toString('utf8').trim();
  if (!message) die('Commit message cannot be empty');
  const result = cp.spawnSync('git', ['commit', '--file', '-'], {
    cwd: root,
    input: message + '\n',
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024
  });
  if (result.status !== 0) {
    die((result.stderr || result.stdout || 'git commit failed').trim());
  }
  console.log(JSON.stringify({ ok: true }));
});
"#;

const GIT_DIFF_SCRIPT: &str = r#"
const root = fs.realpathSync(process.argv[1]);
const rel = process.argv[2] || '';
const section = process.argv[3] || 'unstaged';
if (!rel || path.isAbsolute(rel) || rel.split(/[\\/]+/).includes('..')) die('path escapes workspace');
function show(spec) {
  const out = gitMaybe(['show', spec], { cwd: root, encoding: 'buffer' });
  return textOrNullFromBuffer(out);
}
function worktree(file) {
  const target = path.resolve(root, file);
  ensureInside(root, target);
  if (!fs.existsSync(target)) return null;
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(fs.readFileSync(target));
  } catch (_) {
    return null;
  }
}
function existsInWorktree(file) {
  return fs.existsSync(path.resolve(root, file));
}
function record(oldText, newText, changeType) {
  if (oldText === null && newText === null) return null;
  if ((oldText || '') === (newText || '')) return null;
  return { path: rel, change_type: changeType, old_text: oldText, new_text: newText };
}
let result = null;
if (section === 'staged') {
  const oldText = show(`HEAD:${rel}`);
  const newText = show(`:${rel}`);
  const changeType = oldText === null ? 'Created' : newText === null ? 'Deleted' : 'Modified';
  result = record(oldText, newText, changeType);
} else if (section === 'unstaged') {
  const oldText = show(`:${rel}`) ?? show(`HEAD:${rel}`);
  const newText = worktree(rel);
  const changeType = oldText === null ? 'Created' : newText === null ? 'Deleted' : 'Modified';
  result = record(oldText, newText, changeType);
} else {
  if (show(`HEAD:${rel}`) === null && show(`:${rel}`) === null && existsInWorktree(rel)) {
    result = record(null, worktree(rel), 'Created');
  }
}
console.log(JSON.stringify({ record: result }));
"#;

const SEARCH_SCRIPT: &str = r#"
const root = fs.realpathSync(process.argv[1]);
const query = process.argv[2] || '';
if (!query.trim()) die('Search query must not be empty');
const result = cp.spawnSync('rg', [
  '--json',
  '--no-messages',
  '--max-count',
  '50',
  '--fixed-strings',
  '--ignore-case',
  '--glob',
  '!dist/',
  '--glob',
  '!node_modules/',
  '--glob',
  '!target/',
  '--glob',
  '!build/',
  '--glob',
  '!.git/',
  '--glob',
  '!*.min.js',
  '--glob',
  '!*.min.css',
  '--glob',
  '!*.map',
  '--glob',
  '!*.lock',
  '--',
  query,
  '.'
], {
  cwd: root,
  encoding: 'utf8',
  maxBuffer: 16 * 1024 * 1024
});
const stdout = result.stdout || '';
if (result.error) die(result.error.message || 'failed to execute ripgrep');
if (result.status === 2 && !stdout.trim()) {
  die((result.stderr || 'ripgrep error').trim());
}
const files = new Map();
let total_matches = 0;
let truncated = false;
for (const line of stdout.split(/\r?\n/)) {
  if (total_matches >= 200) {
    truncated = true;
    break;
  }
  if (!line.trim()) continue;
  let value;
  try {
    value = JSON.parse(line);
  } catch (_) {
    continue;
  }
  if (value.type !== 'match' || !value.data) continue;
  const data = value.data;
  const rawPath = data.path && data.path.text;
  const lineNumber = data.line_number;
  const lineText = data.lines && data.lines.text;
  if (!rawPath || !lineNumber || lineText == null) continue;
  const relative = rawPath.replace(/\\/g, '/').replace(/^\.\//, '');
  const matches = files.get(relative) || [];
  matches.push({
    line_number: lineNumber,
    line_text: String(lineText).replace(/\n$/, '')
  });
  files.set(relative, matches);
  total_matches += 1;
}
const sorted = Array.from(files.entries())
  .sort((a, b) => a[0].toLowerCase().localeCompare(b[0].toLowerCase()))
  .map(([path, matches]) => ({ path, matches }));
console.log(JSON.stringify({ query, files: sorted, total_matches, truncated }));
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_ssh::RemoteSshOutput;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeRunner {
        commands: Arc<Mutex<Vec<RemoteSshCommand>>>,
        outputs: Arc<Mutex<Vec<RemoteSshOutput>>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<RemoteSshOutput>) -> Self {
            Self {
                commands: Arc::new(Mutex::new(Vec::new())),
                outputs: Arc::new(Mutex::new(outputs)),
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

    fn config() -> RemoteSshSessionConfig {
        RemoteSshSessionConfig {
            ssh_target: "root@devbox".into(),
            ssh_port: Some(36000),
            remote_workspace_root: "/srv/project".into(),
            local_port: 4000,
            remote_port: 4000,
            ssh_command: None,
            ssh_password: Some("secret".into()),
        }
    }

    fn ok(stdout: &str) -> RemoteSshOutput {
        RemoteSshOutput {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
            timed_out: false,
            elapsed_ms: 1,
        }
    }

    #[test]
    fn list_dir_uses_remote_root_and_decodes_entries() {
        let runner = FakeRunner::new(vec![ok(
            r#"{"entries":[{"name":"src","kind":"Directory","path":"src"},{"name":"Cargo.toml","kind":"File","path":"Cargo.toml"}]}"#,
        )]);
        let config = config();
        let client = RemoteWorkspaceClient::with_runner(&config, runner.clone());

        let entries = client.list_dir("").unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "src");
        let commands = runner.commands();
        assert_eq!(commands[0].ssh_target, "root@devbox");
        assert_eq!(commands[0].ssh_port, Some(36000));
        assert!(commands[0].remote_command.contains("/srv/project"));
    }

    #[test]
    fn save_file_sends_content_over_stdin() {
        let runner = FakeRunner::new(vec![ok(
            r#"{"path":"src/main.rs","content":"new\n","version":{"content_hash":"abc","modified_ms":1,"size":4},"kind":"text","mime_type":null}"#,
        )]);
        let config = config();
        let client = RemoteWorkspaceClient::with_runner(&config, runner.clone());

        let snapshot = client
            .save_file("src/main.rs", "new\n", Some("oldhash"), Some(4), false)
            .unwrap();

        assert_eq!(snapshot.path, "src/main.rs");
        let commands = runner.commands();
        assert_eq!(commands[0].stdin.as_deref(), Some("new\n".as_bytes()));
        assert!(commands[0].remote_command.contains("oldhash"));
    }

    #[test]
    fn rejects_path_traversal_before_ssh() {
        let runner = FakeRunner::new(Vec::new());
        let config = config();
        let client = RemoteWorkspaceClient::with_runner(&config, runner);

        let err = client.read_file("../secret").unwrap_err();

        assert!(err.to_string().contains("路径遍历"));
    }

    #[test]
    fn git_status_decodes_snapshot() {
        let runner = FakeRunner::new(vec![ok(
            r#"{"branch":"main","head":"abc","changed_files":[{"path":"src/main.rs","section":"Unstaged","added":2,"removed":1}]}"#,
        )]);
        let config = config();
        let client = RemoteWorkspaceClient::with_runner(&config, runner);

        let repo = client.git_status().unwrap();

        assert_eq!(repo.branch, "main");
        assert_eq!(repo.changed_files[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(repo.changed_files[0].stats.added, 2);
    }

    #[test]
    fn git_diff_maps_record_to_session_change() {
        let runner = FakeRunner::new(vec![ok(
            r#"{"record":{"path":"src/main.rs","change_type":"Modified","old_text":"old\n","new_text":"new\n"}}"#,
        )]);
        let config = config();
        let client = RemoteWorkspaceClient::with_runner(&config, runner);

        let change = client
            .git_file_diff("src/main.rs", ChangeSection::Unstaged)
            .unwrap()
            .unwrap();

        assert_eq!(change.path, "src/main.rs");
        assert_eq!(change.added_lines, 1);
        assert_eq!(change.removed_lines, 1);
    }

    #[test]
    fn git_unstage_sanitizes_paths_and_runs_remote_reset() {
        let runner = FakeRunner::new(vec![ok(r#"{"ok":true}"#)]);
        let config = config();
        let client = RemoteWorkspaceClient::with_runner(&config, runner.clone());

        client.git_unstage(&["src/main.rs".into()]).unwrap();

        let commands = runner.commands();
        assert!(commands[0].remote_command.contains("reset"));
        assert!(commands[0].remote_command.contains("src/main.rs"));
    }

    #[test]
    fn git_commit_sends_message_over_stdin() {
        let runner = FakeRunner::new(vec![ok(r#"{"ok":true}"#)]);
        let config = config();
        let client = RemoteWorkspaceClient::with_runner(&config, runner.clone());

        client.git_commit("ship remote support").unwrap();

        let commands = runner.commands();
        assert_eq!(
            commands[0].stdin.as_deref(),
            Some("ship remote support\n".as_bytes())
        );
        assert!(commands[0].remote_command.contains("commit"));
    }

    #[test]
    fn search_decodes_remote_results() {
        let runner = FakeRunner::new(vec![ok(
            r#"{"query":"hello","files":[{"path":"src/main.rs","matches":[{"line_number":7,"line_text":"hello world"}]}],"total_matches":1,"truncated":false}"#,
        )]);
        let config = config();
        let client = RemoteWorkspaceClient::with_runner(&config, runner);

        let result = client.search("hello").unwrap();

        assert_eq!(result.query, "hello");
        assert_eq!(result.total_matches, 1);
        assert_eq!(result.files[0].path, "src/main.rs");
        assert_eq!(result.files[0].matches[0].line_number, 7);
    }
}
