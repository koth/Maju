use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const CODEX_ACP_RESOURCE_DIR: &str = "resources/codex-acp";
const CLAUDE_AGENT_ACP_RESOURCE_DIR: &str = "resources/claude-agent-acp";

fn main() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=tauri.release.conf.json");
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/32x32.png");
    println!("cargo:rerun-if-changed=icons/128x128.png");
    println!("cargo:rerun-if-changed=icons/128x128@2x.png");
    println!("cargo:rerun-if-env-changed=KODEX_CODEX_ACP_BINARY");
    println!("cargo:rerun-if-env-changed=KODEX_STAGE_CODEX_ACP");
    println!("cargo:rerun-if-env-changed=KODEX_CLAUDE_AGENT_ACP_BINARY");
    println!("cargo:rerun-if-env-changed=KODEX_STAGE_CLAUDE_AGENT_ACP");
    println!("cargo:rerun-if-env-changed=KODEX_STAGE_CLAUDE_AGENT_ACP_PACKAGE");
    if should_stage_codex_acp_binary() {
        stage_codex_acp_binary();
    }
    if should_stage_claude_agent_acp() {
        stage_claude_agent_acp();
    }
    tauri_build::build()
}

fn should_stage_codex_acp_binary() -> bool {
    env::var("PROFILE").is_ok_and(|profile| profile == "release")
        || env::var_os("KODEX_STAGE_CODEX_ACP").is_some()
}

fn should_stage_claude_agent_acp() -> bool {
    env::var("PROFILE").is_ok_and(|profile| profile == "release")
        || env::var_os("KODEX_STAGE_CLAUDE_AGENT_ACP").is_some()
}





fn stage_claude_agent_acp() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("src-tauri should be nested under apps/desktop");
    let resource_dir = manifest_dir.join(CLAUDE_AGENT_ACP_RESOURCE_DIR);
    if let Err(error) = fs::create_dir_all(&resource_dir) {
        println!(
            "cargo:warning=Could not create claude-agent-acp resource directory {}: {error}",
            resource_dir.display()
        );
        return;
    }
    clear_staged_claude_agent_acp(&resource_dir);

    let prebuilt_binary = workspace_root
        .join("kodex-claude")
        .join("bin")
        .join(claude_agent_acp_binary_name());
    println!("cargo:rerun-if-changed={}", prebuilt_binary.display());
    if let Some(source) = find_claude_agent_acp_binary(workspace_root) {
        stage_claude_agent_acp_file(&source, &resource_dir);
        return;
    }

    if env::var_os("KODEX_STAGE_CLAUDE_AGENT_ACP_PACKAGE").is_some() {
        stage_claude_agent_acp_package(workspace_root, &resource_dir);
        return;
    }

    println!(
        "cargo:warning=No prebuilt claude-agent-acp binary found at {}; run `npm --prefix apps/desktop/ui run claude:binary` before desktop:build, or set KODEX_CLAUDE_AGENT_ACP_BINARY.",
        prebuilt_binary.display()
    );
}

fn stage_claude_agent_acp_package(workspace_root: &Path, resource_dir: &Path) {
    let package_root = workspace_root.join("kodex-claude");
    println!("cargo:rerun-if-changed={}", package_root.display());
    let dist = package_root.join("dist");
    let package_json = package_root.join("package.json");
    let node_modules = package_root.join("node_modules");
    if dist.is_dir() && package_json.is_file() {
        let package_dir = resource_dir.join("package");
        let _ = fs::create_dir_all(&package_dir);
        let _ = copy_dir(&dist, &package_dir.join("dist"));
        let _ = fs::copy(&package_json, package_dir.join("package.json"));
        if node_modules.is_dir() {
            let _ = copy_dir(&node_modules, &package_dir.join("node_modules"));
        }
        write_claude_agent_acp_launchers(&resource_dir);
        println!(
            "cargo:warning=Bundling claude-agent-acp runnable package from {}",
            package_root.display()
        );
    } else {
        println!(
            "cargo:warning=No claude-agent-acp artifact found; Kodex will fall back to npm install."
        );
    }
}

fn find_claude_agent_acp_binary(workspace_root: &Path) -> Option<PathBuf> {
    let binary_name = claude_agent_acp_binary_name();
    let mut candidates = Vec::new();

    if let Ok(path) = env::var("KODEX_CLAUDE_AGENT_ACP_BINARY") {
        candidates.push(PathBuf::from(path));
    }

    candidates.push(
        workspace_root
            .join("kodex-claude")
            .join("bin")
            .join(binary_name),
    );
    candidates.into_iter().find(|path| path.is_file())
}

fn stage_claude_agent_acp_file(source: &Path, resource_dir: &Path) {
    println!("cargo:rerun-if-changed={}", source.display());
    let staged = resource_dir.join(claude_agent_acp_binary_name());
    if let Err(error) = fs::copy(source, &staged) {
        println!(
            "cargo:warning=Could not stage claude-agent-acp from {} to {}: {error}",
            source.display(),
            staged.display()
        );
        return;
    }
    println!(
        "cargo:warning=Bundling claude-agent-acp binary from {}",
        source.display()
    );
}

fn clear_staged_claude_agent_acp(resource_dir: &Path) {
    for name in [
        "claude-agent-acp",
        "claude-agent-acp.exe",
        "claude-agent-acp.cmd",
        "package",
    ] {
        let path = resource_dir.join(name);
        if path.is_dir() {
            let _ = fs::remove_dir_all(path);
        } else if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
}

fn write_claude_agent_acp_launchers(resource_dir: &Path) {
    let unix = resource_dir.join("claude-agent-acp");
    let windows = resource_dir.join("claude-agent-acp.cmd");
    let _ = fs::write(
        unix,
        "#!/usr/bin/env sh\nDIR=\"$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\"\nexec node \"$DIR/package/dist/index.js\" \"$@\"\n",
    );
    let _ = fs::write(
        windows,
        "@echo off\r\nset DIR=%~dp0\r\nnode \"%DIR%package\\dist\\index.js\" %*\r\n",
    );
}

fn copy_dir(source: &Path, target: &Path) -> std::io::Result<()> {
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir(&source_path, &target_path)?;
        } else {
            fs::copy(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn stage_codex_acp_binary() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("src-tauri should be nested under apps/desktop");
    let resource_dir = manifest_dir.join(CODEX_ACP_RESOURCE_DIR);

    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("target").join("release").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root
            .join("codex-acp")
            .join("target")
            .join("release")
            .display()
    );
    if let Ok(target) = env::var("TARGET") {
        println!(
            "cargo:rerun-if-changed={}",
            workspace_root
                .join("target")
                .join(&target)
                .join("release")
                .display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            workspace_root
                .join("codex-acp")
                .join("target")
                .join(target)
                .join("release")
                .display()
        );
    }

    if let Err(error) = fs::create_dir_all(&resource_dir) {
        println!(
            "cargo:warning=Could not create codex-acp resource directory {}: {error}",
            resource_dir.display()
        );
        return;
    }

    clear_staged_codex_acp_binaries(&resource_dir);

    let Some(source) = find_codex_acp_binary(workspace_root) else {
        println!(
            "cargo:warning=No codex-acp binary found under codex-acp/target/release or target/release; run `npm --prefix apps/desktop/ui run codex:binary` before desktop:build, or set KODEX_CODEX_ACP_BINARY."
        );
        return;
    };

    println!("cargo:rerun-if-changed={}", source.display());
    let staged = resource_dir.join(codex_acp_binary_name());
    if let Err(error) = fs::copy(&source, &staged) {
        println!(
            "cargo:warning=Could not stage codex-acp binary from {} to {}: {error}",
            source.display(),
            staged.display()
        );
        return;
    }

    println!(
        "cargo:warning=Bundling codex-acp binary from {}",
        source.display()
    );
}

fn find_codex_acp_binary(workspace_root: &Path) -> Option<PathBuf> {
    let binary_name = codex_acp_binary_name();
    let mut candidates = Vec::new();

    if let Ok(path) = env::var("KODEX_CODEX_ACP_BINARY") {
        candidates.push(PathBuf::from(path));
    }

    if let Ok(target) = env::var("TARGET") {
        candidates.push(
            workspace_root
                .join("codex-acp")
                .join("target")
                .join(&target)
                .join("release")
                .join(binary_name),
        );
        candidates.push(
            workspace_root
                .join("target")
                .join(&target)
                .join("release")
                .join(binary_name),
        );
    }

    candidates.push(
        workspace_root
            .join("codex-acp")
            .join("target")
            .join("release")
            .join(binary_name),
    );

    candidates.push(
        workspace_root
            .join("target")
            .join("release")
            .join(binary_name),
    );

    candidates.into_iter().find(|path| path.is_file())
}

fn clear_staged_codex_acp_binaries(resource_dir: &Path) {
    for name in ["codex-acp", "codex-acp.exe"] {
        let path = resource_dir.join(name);
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
}

fn codex_acp_binary_name() -> &'static str {
    if cfg!(windows) {
        "codex-acp.exe"
    } else {
        "codex-acp"
    }
}

fn claude_agent_acp_binary_name() -> &'static str {
    if cfg!(windows) {
        "claude-agent-acp.exe"
    } else {
        "claude-agent-acp"
    }
}
