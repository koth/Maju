#[tauri::command]
pub fn startup_perf_mark(stage: String, detail: Option<String>) {
    app_core::startup_perf::mark(format!("frontend/{stage}"), detail.unwrap_or_default());
}
