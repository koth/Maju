use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const CODEX_ACP_RESOURCE_DIR: &str = "resources/codex-acp";

fn main() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/32x32.png");
    println!("cargo:rerun-if-changed=icons/128x128.png");
    println!("cargo:rerun-if-changed=icons/128x128@2x.png");
    println!("cargo:rerun-if-env-changed=KODEX_CODEX_ACP_BINARY");
    println!("cargo:rerun-if-env-changed=KODEX_STAGE_CODEX_ACP");
    if should_stage_codex_acp_binary() {
        stage_codex_acp_binary();
    }
    tauri_build::build()
}

fn should_stage_codex_acp_binary() -> bool {
    env::var("PROFILE").is_ok_and(|profile| profile == "release")
        || env::var_os("KODEX_STAGE_CODEX_ACP").is_some()
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
    if let Ok(target) = env::var("TARGET") {
        println!(
            "cargo:rerun-if-changed={}",
            workspace_root
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
            "cargo:warning=No codex-acp binary found under target/release; Kodex will fall back to npm install."
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
                .join("target")
                .join(target)
                .join("release")
                .join(binary_name),
        );
    }

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
