use super::*;

#[test]
fn unix_drive_prefix_paths_are_normalized_for_workspace_file_reads() {
    assert_eq!(
        normalize_unix_drive_prefix("/d/work/ArtAssets/docs/tags.md"),
        "D:/work/ArtAssets/docs/tags.md"
    );
    assert_eq!(
        normalize_unix_drive_prefix("/mnt/d/work/ArtAssets/docs/tags.md"),
        "D:/work/ArtAssets/docs/tags.md"
    );
}

#[test]
fn edit_preview_new_text_can_be_rebuilt_from_raw_input_content() {
    let raw_input = serde_json::json!({
        "path": "/workspace/README.md",
        "before": "## Project Structure\n",
        "after": "## 项目结构\n",
        "content": "# Kodex\n\n## Project Structure\n\nbody\n"
    });

    assert_eq!(
        edit_preview_new_text_from_raw_input(Some(&raw_input)).as_deref(),
        Some("# Kodex\n\n## 项目结构\n\nbody\n")
    );
}
