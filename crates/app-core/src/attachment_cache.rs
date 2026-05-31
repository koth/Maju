use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use workspace_model::UserPromptContent;

const ATTACHMENT_CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub(crate) fn cache_prompt_images(
    prompt: &mut [UserPromptContent],
    attachments_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(attachments_dir)
        .with_context(|| format!("创建图片附件缓存目录 {} 失败", attachments_dir.display()))?;
    prune_expired_attachments(attachments_dir)?;

    for content in prompt {
        let UserPromptContent::Image {
            data,
            mime_type,
            display_url,
            ..
        } = content
        else {
            continue;
        };
        if display_url.is_some() {
            continue;
        }

        let bytes = base64_decode(data)?;
        let ext = extension_for_mime_type(mime_type);
        let path = attachments_dir.join(format!("{}.{}", uuid::Uuid::new_v4(), ext));
        std::fs::write(&path, bytes)
            .with_context(|| format!("写入图片附件缓存 {} 失败", path.display()))?;
        *display_url = Some(file_uri(&path));
    }

    Ok(())
}

pub(crate) fn prune_expired_attachments(attachments_dir: &Path) -> Result<()> {
    prune_expired_attachments_at(attachments_dir, SystemTime::now())
}

fn prune_expired_attachments_at(attachments_dir: &Path, now: SystemTime) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(attachments_dir) else {
        return Ok(());
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now
            .duration_since(modified)
            .is_ok_and(|age| age > ATTACHMENT_CACHE_TTL)
        {
            let _ = std::fs::remove_file(path);
        }
    }

    Ok(())
}

fn extension_for_mime_type(mime_type: &str) -> &'static str {
    match mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/apng" => "apng",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/gif" => "gif",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/svg+xml" => "svg",
        "image/webp" => "webp",
        _ => "bin",
    }
}

fn file_uri(path: &Path) -> String {
    let path = absolute_path(path);
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{}", percent_encode_path(&normalized))
    } else {
        format!("file:///{}", percent_encode_path(&normalized))
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn percent_encode_path(path: &str) -> String {
    path.split('/')
        .enumerate()
        .map(|(index, segment)| {
            if index == 0 && segment.is_empty() {
                String::new()
            } else {
                percent_encode(segment)
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b':') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn base64_decode(input: &str) -> Result<Vec<u8>> {
    let mut values = Vec::with_capacity(input.len());
    let mut padding = 0usize;
    for byte in input.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        match byte {
            b'A'..=b'Z' => values.push(byte - b'A'),
            b'a'..=b'z' => values.push(byte - b'a' + 26),
            b'0'..=b'9' => values.push(byte - b'0' + 52),
            b'+' => values.push(62),
            b'/' => values.push(63),
            b'=' => {
                values.push(0);
                padding += 1;
            }
            _ => return Err(anyhow!("图片附件包含无效 base64 数据")),
        }
    }

    if values.len() % 4 != 0 {
        return Err(anyhow!("图片附件 base64 长度无效"));
    }
    if padding > 2 {
        return Err(anyhow!("图片附件 base64 padding 无效"));
    }

    let mut output = Vec::with_capacity(values.len() / 4 * 3);
    for chunk in values.chunks(4) {
        let n = ((chunk[0] as u32) << 18)
            | ((chunk[1] as u32) << 12)
            | ((chunk[2] as u32) << 6)
            | (chunk[3] as u32);
        output.push((n >> 16) as u8);
        output.push((n >> 8) as u8);
        output.push(n as u8);
    }
    for _ in 0..padding {
        output.pop();
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{FileTime, set_file_mtime};
    use tempfile::tempdir;

    #[test]
    fn caches_prompt_image_and_sets_display_url() {
        let dir = tempdir().unwrap();
        let mut prompt = vec![UserPromptContent::image(
            "aW1hZ2U=",
            "image/png",
            Some("sample.png".into()),
        )];

        cache_prompt_images(&mut prompt, dir.path()).unwrap();

        let UserPromptContent::Image { display_url, .. } = &prompt[0] else {
            panic!("expected image content");
        };
        let display_url = display_url.as_deref().expect("display url should be set");
        assert!(display_url.starts_with("file://"));
        assert!(display_url.ends_with(".png"));
        let cached_files = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(cached_files, 1);
    }

    #[test]
    fn prunes_cached_files_after_ttl() {
        let dir = tempdir().unwrap();
        let fresh = dir.path().join("fresh.png");
        let stale = dir.path().join("stale.png");
        std::fs::write(&fresh, b"fresh").unwrap();
        std::fs::write(&stale, b"stale").unwrap();
        let now = SystemTime::now();
        let stale_time =
            FileTime::from_system_time(now - ATTACHMENT_CACHE_TTL - Duration::from_secs(1));
        set_file_mtime(&stale, stale_time).unwrap();

        prune_expired_attachments_at(dir.path(), now).unwrap();

        assert!(fresh.exists());
        assert!(!stale.exists());
    }
}
