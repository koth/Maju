//! Image API client backing the `kodex-image` MCP tools.
//!
//! Two independent model pipelines (per design D15):
//! - `view_image` reuses the existing provider system: it routes a multimodal
//!   chat request through the local `codex_api_proxy` Responses endpoint, so
//!   provider-specific auth/headers (e.g. TimiAI) are handled by the proxy for
//!   free. The view model + provider come from `settings.image.view`
//!   (a catalog multimodal model).
//! - `generate_image` and `edit_image` share one independently-configured
//!   generation model (`settings.image.generate`) whose wire protocol is
//!   selected by `ImageGenerateProtocol`: OpenAI `images/generations` /
//!   `images/edits`, OpenAI `chat/completions` (inline image output), or
//!   Google Gemini `generateContent`. `edit_image` passes the original image
//!   directly to the generation model and never routes through `view_image`
//!   (per design D9).
//!
//! Results use the native `ImageContent { data, mime_type, uri }` shape plus a
//! `saved_path`, aligned with `image_generation_content` so the reducer and
//! frontend render them without special handling (per design / spec).

use acp_core::codex_api_proxy_base_url;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::image_mcp::ImageMcpConfig;

/// In-memory description cache for `view_image`, keyed by image hash + question.
/// Lives on the shared `ImageMcpService` so it persists across tool calls.
pub(crate) type ViewCache = HashMap<String, String>;

pub struct ImageApi {
    config: ImageMcpConfig,
    view_cache: Arc<Mutex<ViewCache>>,
}

impl ImageApi {
    pub fn new(config: ImageMcpConfig, view_cache: Arc<Mutex<ViewCache>>) -> Self {
        Self { config, view_cache }
    }

    /// Understand an image: read it from `image_path`, send it with an optional
    /// question to the configured multimodal view model via the codex API
    /// proxy, and return a text description.
    pub async fn view_image(&self, arguments: &Value) -> Result<Value, String> {
        let image_path = arguments
            .get("image_path")
            .and_then(Value::as_str)
            .ok_or_else(|| "view_image requires `image_path`".to_string())?;
        let question = arguments
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or("");
        let model = self.config.settings.view.model.trim();
        let provider = self.config.settings.view.provider.trim();
        if model.is_empty() || provider.is_empty() {
            return Err(
                "image.view.model/provider is not configured; cannot run view_image".to_string(),
            );
        }

        let path = crate::attachment_cache::local_path_from_uri(image_path)
            .ok_or_else(|| format!("view_image could not resolve a local path from `{image_path}`"))?;
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("failed to read image {path:?}: {error}"))?;
        let mime = mime_for_path(&path);
        let hash = image_hash(&bytes);
        let cache_key = format!("{hash}:{question}");

        if let Some(cached) = self.view_cache_get(&cache_key) {
            return Ok(json!({
                "description": cached,
                "details": null,
                "cached": true
            }));
        }

        let data_url = data_url(&bytes, mime);
        let prompt_text = if question.trim().is_empty() {
            "Describe this image in detail.".to_string()
        } else {
            question.to_string()
        };
        let payload = json!({
            "model": model,
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": prompt_text},
                    {"type": "input_image", "image_url": data_url}
                ]
            }]
        });

        // Register the view provider's key without disturbing the active
        // session's proxy provider (the proxy routes by the provider pinned in
        // the request path, and resolves this key via api_key_for_proxy_provider).
        if let Some(api_key) = self.config.view_api_key.as_deref() {
            acp_core::register_codex_api_proxy_provider_key(provider, api_key);
        }

        let url = format!("{}/providers/{provider}/responses", codex_api_proxy_base_url());
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|error| format!("view_image request failed: {error}"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("view_image could not parse response ({status}): {error}"))?;
        if !status.is_success() {
            let message = error_message(&body);
            return Err(format!(
                "view_image multimodal call failed ({status}): {message}. \
                 Ensure the proxy is running and `{provider}` has a configured API key."
            ));
        }

        let description = extract_responses_output_text(&body).ok_or_else(|| {
            format!(
                "view_image returned no text output. Raw response: {}",
                truncate(&body.to_string(), 500)
            )
        })?;
        let description = description.trim().to_string();
        self.view_cache_set(cache_key, description.clone());
        Ok(json!({
            "description": description,
            "details": null,
            "cached": false
        }))
    }

    /// Generate a new image from `prompt` and persist each result under
    /// `.kodex/generated-images/`.
    pub async fn generate_image(&self, arguments: &Value) -> Result<Value, String> {
        let prompt = arguments
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| "generate_image requires `prompt`".to_string())?;
        let size = arguments
            .get("size")
            .and_then(Value::as_str)
            .filter(|size| !size.trim().is_empty())
            .unwrap_or(&self.config.settings.generate.default_size);
        let n = arguments
            .get("n")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .clamp(1, 4) as usize;

        let generate = &self.config.settings.generate;
        let (model, base_url, api_key) =
            generate_endpoints(generate, self.config.generate_api_key.as_deref())?;
        let images = match generate.protocol {
            workspace_model::ImageGenerateProtocol::OpenaiImages => {
                generate_openai_images(&base_url, &api_key, &model, prompt, size, n).await?
            }
            workspace_model::ImageGenerateProtocol::ChatCompletions => {
                generate_chat_completions(&base_url, &api_key, &model, prompt, size).await?
            }
            workspace_model::ImageGenerateProtocol::Gemini => {
                generate_gemini(&base_url, &api_key, &model, prompt, size).await?
            }
        };
        if images.is_empty() {
            return Err(format!(
                "generate_image returned no images. Raw response: {}",
                "<no images>"
            ));
        }

        let dir = self.generated_images_dir();
        let mut results = Vec::with_capacity(images.len());
        for image in images {
            let saved = persist_image(&dir, &image.data, image.mime)?;
            results.push(json!({
                "path": saved.uri,
                "saved_path": saved.path,
                "revised_prompt": image.revised_prompt,
                "mime_type": image.mime
            }));
        }
        Ok(json!({
            "images": results,
            "saved_dir": dir.display().to_string()
        }))
    }

    /// Edit an existing image: read the original from `image_path`, pass it
    /// with `prompt` directly to the generation model via `images/edits`, and
    /// persist the result. Does not route through `view_image`.
    pub async fn edit_image(&self, arguments: &Value) -> Result<Value, String> {
        let image_path = arguments
            .get("image_path")
            .and_then(Value::as_str)
            .ok_or_else(|| "edit_image requires `image_path`".to_string())?;
        let prompt = arguments
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| "edit_image requires `prompt`".to_string())?;
        let mask_path = arguments.get("mask_path").and_then(Value::as_str);

        let generate = &self.config.settings.generate;
        let (model, base_url, api_key) =
            generate_endpoints(generate, self.config.generate_api_key.as_deref())?;

        let source = crate::attachment_cache::local_path_from_uri(image_path)
            .ok_or_else(|| format!("edit_image could not resolve a local path from `{image_path}`"))?;
        let image_bytes = std::fs::read(&source)
            .map_err(|error| format!("failed to read source image {source:?}: {error}"))?;
        let image_mime = mime_for_path(&source);
        let size = generate.default_size.trim();
        let images = match generate.protocol {
            workspace_model::ImageGenerateProtocol::OpenaiImages => {
                edit_openai_images(
                    &base_url,
                    &api_key,
                    &model,
                    &prompt,
                    size,
                    &image_bytes,
                    image_mime,
                    mask_path,
                )
                .await?
            }
            workspace_model::ImageGenerateProtocol::ChatCompletions => {
                edit_chat_completions(&base_url, &api_key, &model, &prompt, &image_bytes, image_mime)
                    .await?
            }
            workspace_model::ImageGenerateProtocol::Gemini => {
                edit_gemini(&base_url, &api_key, &model, &prompt, &image_bytes, image_mime)
                    .await?
            }
        };
        let image = images
            .into_iter()
            .next()
            .ok_or_else(|| "edit_image returned no image".to_string())?;
        let dir = self.generated_images_dir();
        let saved = persist_image(&dir, &image.data, image.mime)?;
        Ok(json!({
            "path": saved.uri,
            "saved_path": saved.path,
            "revised_prompt": image.revised_prompt,
            "mime_type": image.mime,
            "source": image_path
        }))
    }

    fn generated_images_dir(&self) -> PathBuf {
        self.config
            .workspace_root
            .join(".kodex")
            .join("generated-images")
    }

    fn view_cache_get(&self, key: &str) -> Option<String> {
        self.view_cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(key).cloned())
    }

    fn view_cache_set(&self, key: String, value: String) {
        if let Ok(mut cache) = self.view_cache.lock() {
            cache.insert(key, value);
        }
    }
}

struct DecodedImage {
    data: Vec<u8>,
    mime: &'static str,
    revised_prompt: Option<String>,
}

struct PersistedImage {
    uri: String,
    path: String,
}

/// Resolve the configured generation model, base URL, and API key, returning a
/// descriptive error when any required piece is missing.
fn generate_endpoints(
    generate: &workspace_model::ImageGenerateSettings,
    api_key: Option<&str>,
) -> Result<(String, String, String), String> {
    let model = generate.model.trim().to_string();
    let base_url = generate.base_url.trim().trim_end_matches('/').to_string();
    if model.is_empty() || base_url.is_empty() {
        return Err(
            "image.generate.model/base_url is not configured; cannot run generate_image".to_string(),
        );
    }
    let api_key = api_key
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| "image.generate API key is not configured".to_string())?
        .to_string();
    Ok((model, base_url, api_key))
}

/// OpenAI-compatible `POST /images/generations`.
async fn generate_openai_images(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    size: &str,
    n: usize,
) -> Result<Vec<DecodedImage>, String> {
    let payload = json!({
        "model": model,
        "prompt": prompt,
        "size": size,
        "n": n,
        "response_format": "b64_json"
    });
    let url = format!("{base_url}/images/generations");
    let response = post_json(&url, api_key, &payload)
        .await
        .map_err(|error| format!("generate_image request failed: {error}"))?;
    parse_image_results(&response).await
}

/// OpenAI-compatible `POST /images/edits` (multipart).
async fn edit_openai_images(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    size: &str,
    image_bytes: &[u8],
    image_mime: &str,
    mask_path: Option<&str>,
) -> Result<Vec<DecodedImage>, String> {
    let image_ext = crate::attachment_cache::extension_for_mime_type(image_mime);
    let mut form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .text("prompt", prompt.to_string())
        .text("response_format", "b64_json".to_string());
    if !size.is_empty() {
        form = form.text("size", size.to_string());
    }
    let image_part = reqwest::multipart::Part::bytes(image_bytes.to_vec())
        .file_name(format!("image.{image_ext}"))
        .mime_str(image_mime)
        .map_err(|error| format!("edit_image invalid image mime: {error}"))?;
    form = form.part("image", image_part);
    if let Some(mask_path) = mask_path.filter(|path| !path.trim().is_empty()) {
        let mask_source = crate::attachment_cache::local_path_from_uri(mask_path)
            .ok_or_else(|| format!("edit_image could not resolve mask path from `{mask_path}`"))?;
        let mask_bytes = std::fs::read(&mask_source)
            .map_err(|error| format!("failed to read mask {mask_source:?}: {error}"))?;
        let mask_part = reqwest::multipart::Part::bytes(mask_bytes)
            .file_name("mask.png")
            .mime_str("image/png")
            .map_err(|error| format!("edit_image invalid mask mime: {error}"))?;
        form = form.part("mask", mask_part);
    }

    let url = format!("{base_url}/images/edits");
    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|error| format!("edit_image request failed: {error}"))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("edit_image could not parse response ({status}): {error}"))?;
    if !status.is_success() {
        let message = error_message(&body);
        return Err(format!("edit_image call failed ({status}): {message}"));
    }
    parse_image_results(&body).await
}

/// OpenAI-compatible `POST /chat/completions` for image generation. The model
/// is asked to emit an image; image output is recovered from inline
/// `image_url` content parts or a `data` array in the response.
async fn generate_chat_completions(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    size: &str,
) -> Result<Vec<DecodedImage>, String> {
    let mut user_content = format!("Generate an image. Prompt: {prompt}");
    if !size.is_empty() {
        user_content.push_str(&format!("\nSize: {size}"));
    }
    let payload = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": user_content
        }],
        "stream": false
    });
    let url = format!("{base_url}/chat/completions");
    let response = post_json(&url, api_key, &payload)
        .await
        .map_err(|error| format!("generate_image (chat) request failed: {error}"))?;
    parse_chat_image_response(&response).await
}

/// `POST /chat/completions` for image editing: the original image is sent as
/// an `image_url` data-URL content part alongside the edit prompt.
async fn edit_chat_completions(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    image_bytes: &[u8],
    image_mime: &str,
) -> Result<Vec<DecodedImage>, String> {
    let data_url = data_url(image_bytes, image_mime);
    let payload = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "image_url", "image_url": {"url": data_url}}
            ]
        }],
        "stream": false
    });
    let url = format!("{base_url}/chat/completions");
    let response = post_json(&url, api_key, &payload)
        .await
        .map_err(|error| format!("edit_image (chat) request failed: {error}"))?;
    parse_chat_image_response(&response).await
}

/// Google Gemini `POST /models/{model}:generateContent` with
/// `responseModalities: ["IMAGE", "TEXT"]`.
///
/// The API key is sent both as a `?key=` query parameter (Gemini Developer
/// API) and as an `x-goog-api-key` header (Vertex AI and third-party Vertex
/// proxies such as zenmux, which ignore the query param and otherwise return
/// 403). The `google-genai` SDK sends the same `x-goog-api-key` header when
/// `vertexai=True`, so this matches the documented wire format. `role: "user"`
/// is set on `contents` because Vertex proxies reject roleless contents with
/// `Please use a valid role: user, model.`
async fn generate_gemini(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    _size: &str,
) -> Result<Vec<DecodedImage>, String> {
    let payload = json!({
        "contents": [{
            "role": "user",
            "parts": [{"text": prompt}]
        }],
        "generationConfig": {
            "responseModalities": ["IMAGE", "TEXT"]
        }
    });
    let url = format!(
        "{base_url}/models/{model}:generateContent?key={}",
        percent_encode_query(api_key)
    );
    let response = post_json_gemini(&url, api_key, &payload)
        .await
        .map_err(|error| format!("generate_image (gemini) request failed: {error}"))?;
    parse_gemini_image_response(&response)
}

/// Gemini edit: send the original image as an `inline_data` part with the
/// edit prompt, requesting image output modalities. See `generate_gemini` for
/// why the key is sent via the `x-goog-api-key` header and `role: "user"` is
/// required on `contents`.
async fn edit_gemini(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    image_bytes: &[u8],
    image_mime: &str,
) -> Result<Vec<DecodedImage>, String> {
    let payload = json!({
        "contents": [{
            "role": "user",
            "parts": [
                {"text": prompt},
                {"inline_data": {"mime_type": image_mime, "data": BASE64.encode(image_bytes)}}
            ]
        }],
        "generationConfig": {
            "responseModalities": ["IMAGE", "TEXT"]
        }
    });
    let url = format!(
        "{base_url}/models/{model}:generateContent?key={}",
        percent_encode_query(api_key)
    );
    let response = post_json_gemini(&url, api_key, &payload)
        .await
        .map_err(|error| format!("edit_image (gemini) request failed: {error}"))?;
    parse_gemini_image_response(&response)
}

/// Extract images from an OpenAI `chat/completions` response. Handles both the
/// OpenAI Images-in-Chat shape (`message.images[]` with `b64_json`) and inline
/// `image_url` content parts, falling back to a `data` array.
async fn parse_chat_image_response(response: &Value) -> Result<Vec<DecodedImage>, String> {
    // OpenAI images-in-chat: choices[0].message.images[]
    if let Some(images) = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("images"))
        .and_then(Value::as_array)
    {
        let mut out = Vec::new();
        for image in images {
            if let Some(b64) = image.get("b64_json").and_then(Value::as_str) {
                let bytes = BASE64
                    .decode(b64)
                    .map_err(|error| format!("failed to decode b64_json: {error}"))?;
                out.push(DecodedImage {
                    data: bytes,
                    mime: "image/png",
                    revised_prompt: image
                        .get("revised_prompt")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            } else if let Some(url) = image.get("url").and_then(Value::as_str) {
                let bytes = fetch_image_url(url)
                    .await
                    .map_err(|error| format!("failed to fetch image url {url}: {error}"))?;
                let mime = mime_from_bytes(&bytes);
                out.push(DecodedImage {
                    data: bytes,
                    mime,
                    revised_prompt: None,
                });
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    // Inline image_url content parts.
    if let Some(content) = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
    {
        let mut out = Vec::new();
        if let Some(parts) = content.as_array() {
            for part in parts {
                if part.get("type").and_then(Value::as_str) == Some("image_url") {
                    if let Some(url) = part
                        .get("image_url")
                        .and_then(|iu| iu.get("url"))
                        .and_then(Value::as_str)
                    {
                        if let Some(bytes) = decode_data_url(url) {
                            out.push(DecodedImage {
                                data: bytes.0,
                                mime: bytes.1,
                                revised_prompt: None,
                            });
                        } else {
                            let fetched = fetch_image_url(url)
                                .await
                                .map_err(|error| format!("fetch image url {url}: {error}"))?;
                            let mime = mime_from_bytes(&fetched);
                            out.push(DecodedImage {
                                data: fetched,
                                mime,
                                revised_prompt: None,
                            });
                        }
                    }
                }
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    // Fall back to the standard images `data[]` shape.
    parse_image_results(response).await
}

/// Extract images from a Gemini `generateContent` response:
/// `candidates[0].content.parts[]` with `inline_data { mimeType, data }`.
fn parse_gemini_image_response(response: &Value) -> Result<Vec<DecodedImage>, String> {
    let parts = response
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "gemini response has no image parts. Raw: {}",
                truncate(&response.to_string(), 500)
            )
        })?;
    let mut out = Vec::new();
    for part in parts {
        if let Some(inline) = part.get("inline_data").or_else(|| part.get("inlineData")) {
            if let (Some(b64), Some(mime)) = (
                inline.get("data").and_then(Value::as_str),
                inline
                    .get("mimeType")
                    .or_else(|| inline.get("mime_type"))
                    .and_then(Value::as_str),
            ) {
                let bytes = BASE64
                    .decode(b64)
                    .map_err(|error| format!("failed to decode gemini inline_data: {error}"))?;
                out.push(DecodedImage {
                    data: bytes,
                    mime: mime_from_str(mime),
                    revised_prompt: None,
                });
            }
        }
    }
    if out.is_empty() {
        return Err(format!(
            "gemini response contained no inline_data image. Raw: {}",
            truncate(&response.to_string(), 500)
        ));
    }
    Ok(out)
}

/// Decode a `data:{mime};base64,{data}` URL into (bytes, mime).
fn decode_data_url(url: &str) -> Option<(Vec<u8>, &'static str)> {
    let rest = url.strip_prefix("data:")?;
    let (mime, b64) = rest.split_once(";base64,")?;
    let bytes = BASE64.decode(b64).ok()?;
    Some((bytes, mime_from_str(mime)))
}

/// Map a MIME string to a static lifetime. Falls back to `image/png`.
fn mime_from_str(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/bmp" => "image/bmp",
        "image/svg+xml" => "image/svg+xml",
        _ => "image/png",
    }
}

/// Parse the `data[]` array from an OpenAI-compatible `images/generations` or
/// `images/edits` response. Supports `b64_json` (preferred, requested) and
/// falls back to fetching a `url` when the provider does not honor
/// `response_format`.
async fn parse_image_results(response: &Value) -> Result<Vec<DecodedImage>, String> {
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "response is missing `data` array".to_string())?;
    let mut images = Vec::with_capacity(data.len());
    for entry in data {
        let revised_prompt = entry
            .get("revised_prompt")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(b64) = entry.get("b64_json").and_then(Value::as_str) {
            let bytes = BASE64
                .decode(b64)
                .map_err(|error| format!("failed to decode b64_json: {error}"))?;
            images.push(DecodedImage {
                data: bytes,
                mime: "image/png",
                revised_prompt,
            });
        } else if let Some(url) = entry.get("url").and_then(Value::as_str) {
            let bytes = fetch_image_url(url)
                .await
                .map_err(|error| format!("failed to fetch image url {url}: {error}"))?;
            let mime = mime_from_bytes(&bytes);
            images.push(DecodedImage {
                data: bytes,
                mime,
                revised_prompt,
            });
        }
    }
    Ok(images)
}

async fn fetch_image_url(url: &str) -> Result<Vec<u8>, String> {
    let bytes = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .bytes()
        .await
        .map_err(|error| error.to_string())?;
    Ok(bytes.to_vec())
}

async fn post_json(url: &str, api_key: &str, payload: &Value) -> Result<Value, String> {
    let client = reqwest::Client::new();
    let mut request = client.post(url).json(payload);
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let response = request.send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("could not parse response ({status}): {error}"))?;
    if !status.is_success() {
        let message = error_message(&body);
        return Err(format!("request to {url} failed ({status}): {message}"));
    }
    Ok(body)
}

/// Like `post_json`, but authenticates with the `x-goog-api-key` header
/// instead of `Authorization: Bearer`. The Gemini Developer API and Vertex AI
/// both accept this header; Vertex proxies (e.g. zenmux) require it because
/// they ignore the `?key=` query parameter.
async fn post_json_gemini(url: &str, api_key: &str, payload: &Value) -> Result<Value, String> {
    let client = reqwest::Client::new();
    let mut request = client.post(url).json(payload);
    if !api_key.is_empty() {
        request = request.header("x-goog-api-key", api_key);
    }
    let response = request.send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("could not parse response ({status}): {error}"))?;
    if !status.is_success() {
        let message = error_message(&body);
        return Err(format!("request to {url} failed ({status}): {message}"));
    }
    Ok(body)
}
/// Percent-encode a value for use in a URL query string (Gemini API key).
fn percent_encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Extract a human-readable error message from a JSON error body, falling
/// back to the raw body string when no `error.message` field is present.
fn error_message(body: &Value) -> String {
    body.get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| body.to_string())
}

/// Persist generated image bytes to `{dir}/{uuid}.{ext}` and return the
/// `file://` URI plus the absolute saved path.
fn persist_image(dir: &Path, bytes: &[u8], mime: &str) -> Result<PersistedImage, String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    let ext = extension_for_mime(mime);
    let path = dir.join(format!("{}.{}", uuid::Uuid::new_v4(), ext));
    std::fs::write(&path, bytes)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    let uri = crate::attachment_cache::file_uri(&path);
    Ok(PersistedImage {
        uri,
        path: path.to_string_lossy().to_string(),
    })
}

/// Extract the assistant text from an OpenAI Responses-format response
/// (`output[].content[]` with `type: "output_text"`), defensively handling
/// shape variations across providers proxied through codex_api_proxy.
fn extract_responses_output_text(response: &Value) -> Option<String> {
    let output = response.get("output").and_then(Value::as_array)?;
    let mut texts = Vec::new();
    for item in output {
        let content = item.get("content").and_then(Value::as_array)?;
        for part in content {
            if matches!(part.get("type").and_then(Value::as_str), Some("output_text")) {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        texts.push(text.to_string());
                    }
                }
            }
        }
    }
    if texts.is_empty() {
        return None;
    }
    Some(texts.join("\n"))
}

fn data_url(bytes: &[u8], mime: &str) -> String {
    format!("data:{mime};base64,{}", BASE64.encode(bytes))
}

fn image_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        _ => "image/png",
    }
}

fn extension_for_mime(mime: &str) -> &'static str {
    crate::attachment_cache::extension_for_mime_type(mime)
}

/// Best-effort MIME sniff from image magic bytes.
fn mime_from_bytes(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.len() > 11 && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<svg") {
        "image/svg+xml"
    } else {
        "image/png"
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_string()
    } else {
        format!("{}…", &value[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn config() -> ImageMcpConfig {
        ImageMcpConfig {
            workspace_root: std::env::temp_dir(),
            settings: workspace_model::ImageSettings::default(),
            view_api_key: None,
            generate_api_key: None,
        }
    }

    fn api() -> ImageApi {
        ImageApi::new(config(), Arc::new(Mutex::new(ViewCache::default())))
    }

    #[test]
    fn data_url_round_trips_png() {
        let bytes = [0x89, 0x50, 0x4E, 0x47, 0x0D];
        let url = data_url(&bytes, "image/png");
        assert!(url.starts_with("data:image/png;base64,"));
        let b64 = &url["data:image/png;base64,".len()..];
        assert_eq!(BASE64.decode(b64).unwrap(), bytes);
    }

    #[test]
    fn image_hash_is_stable_and_distinct() {
        let a = image_hash(&[1, 2, 3]);
        let b = image_hash(&[1, 2, 3]);
        let c = image_hash(&[1, 2, 4]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.len() == 64); // sha256 hex
    }

    #[test]
    fn extract_responses_output_text_joins_output_text_parts() {
        let response = json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "output_text", "text": "A cat."},
                    {"type": "output_text", "text": "It is orange."}
                ]
            }]
        });
        assert_eq!(
            extract_responses_output_text(&response).as_deref(),
            Some("A cat.\nIt is orange.")
        );
    }

    #[test]
    fn extract_responses_output_text_returns_none_when_empty() {
        assert!(extract_responses_output_text(&json!({"output": []})).is_none());
        assert!(extract_responses_output_text(&json!({"output": [{"content": [{"type": "output_text", "text": "  "}]}]})).is_none());
    }

    #[test]
    fn persist_image_writes_file_and_returns_uri() {
        let dir = std::env::temp_dir().join(format!("kodex-image-test-{}", uuid::Uuid::new_v4()));
        let bytes = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A];
        let saved = persist_image(&dir, &bytes, "image/png").unwrap();
        assert!(saved.uri.starts_with("file://"));
        assert!(saved.path.ends_with(".png"));
        let on_disk = std::fs::read(&saved.path).unwrap();
        assert_eq!(on_disk, bytes);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn run_async<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn parse_image_results_decodes_b64_json() {
        let response = json!({
            "data": [
                {"b64_json": BASE64.encode(&[1, 2, 3]), "revised_prompt": "a cat"},
                {"b64_json": BASE64.encode(&[4, 5, 6])}
            ]
        });
        let images = run_async(parse_image_results(&response)).unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].data, vec![1, 2, 3]);
        assert_eq!(images[0].mime, "image/png");
        assert_eq!(images[0].revised_prompt.as_deref(), Some("a cat"));
        assert!(images[1].revised_prompt.is_none());
    }

    #[test]
    fn parse_image_results_errors_without_data() {
        assert!(run_async(parse_image_results(&json!({"foo": 1}))).is_err());
    }

    #[test]
    fn mime_sniff_matches_magic_bytes() {
        assert_eq!(mime_from_bytes(&[0x89, 0x50, 0x4E, 0x47, 0x0D]), "image/png");
        assert_eq!(mime_from_bytes(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(mime_from_bytes(b"GIF89a"), "image/gif");
        let webp = [b'R', b'I', b'F', b'F', 0, 0, 0, 0, b'W', b'E', b'B', b'P'];
        assert_eq!(mime_from_bytes(&webp), "image/webp");
    }

    #[test]
    fn view_cache_persists_across_calls_on_shared_arc() {
        let cache: Arc<Mutex<ViewCache>> = Arc::new(Mutex::new(ViewCache::default()));
        let api = ImageApi::new(config(), cache.clone());
        api.view_cache_set("k".into(), "v".into());
        assert_eq!(api.view_cache_get("k"), Some("v".to_string()));
        // A second ImageApi sharing the same Arc sees the cached value.
        let api2 = ImageApi::new(config(), cache);
        assert_eq!(api2.view_cache_get("k"), Some("v".to_string()));
    }

    #[test]
    fn local_path_from_uri_round_trips_file_uri() {
        use crate::attachment_cache::file_uri;
        let tmp = std::env::temp_dir().join("kodex-roundtrip.png");
        std::fs::write(&tmp, b"x").unwrap();
        let uri = file_uri(&tmp);
        let parsed = crate::attachment_cache::local_path_from_uri(&uri).unwrap();
        // Normalize separators for cross-platform comparison.
        let a = tmp.to_string_lossy().replace('\\', "/");
        let b = parsed.to_string_lossy().replace('\\', "/");
        assert_eq!(a.to_ascii_lowercase(), b.to_ascii_lowercase());
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn local_path_from_uri_accepts_plain_path() {
        let parsed = crate::attachment_cache::local_path_from_uri("/tmp/img.png");
        assert_eq!(parsed, Some(PathBuf::from("/tmp/img.png")));
    }

    #[test]
    fn generate_image_requires_configured_model_and_base_url() {
        let api = api();
        let arguments = json!({"prompt": "x"});
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = runtime.block_on(api.generate_image(&arguments)).unwrap_err();
        assert!(err.contains("not configured"), "{err}");
    }

    #[test]
    fn edit_image_requires_configured_model_and_base_url() {
        let api = api();
        let arguments = json!({"image_path": "file:///x.png", "prompt": "x"});
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = runtime.block_on(api.edit_image(&arguments)).unwrap_err();
        assert!(err.contains("not configured"), "{err}");
    }

    #[test]
    fn view_image_requires_configured_view_model() {
        let api = api();
        let arguments = json!({"image_path": "file:///x.png"});
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = runtime.block_on(api.view_image(&arguments)).unwrap_err();
        assert!(err.contains("not configured"), "{err}");
    }

    #[test]
    fn parse_gemini_image_response_extracts_inline_data() {
        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "edited image"},
                        {"inline_data": {"mimeType": "image/png", "data": BASE64.encode(&[1, 2, 3])}}
                    ]
                }
            }]
        });
        let images = parse_gemini_image_response(&response).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].data, vec![1, 2, 3]);
        assert_eq!(images[0].mime, "image/png");
    }

    #[test]
    fn parse_gemini_image_response_accepts_snake_case_inline_data() {
        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"inline_data": {"mime_type": "image/jpeg", "data": BASE64.encode(&[9])}}
                    ]
                }
            }]
        });
        let images = parse_gemini_image_response(&response).unwrap();
        assert_eq!(images[0].mime, "image/jpeg");
        assert_eq!(images[0].data, vec![9]);
    }

    #[test]
    fn parse_gemini_image_response_errors_without_image() {
        let response = json!({
            "candidates": [{"content": {"parts": [{"text": "no image here"}]}}]
        });
        assert!(parse_gemini_image_response(&response).is_err());
    }

    #[test]
    fn parse_chat_image_response_extracts_message_images() {
        let response = json!({
            "choices": [{
                "message": {
                    "images": [
                        {"b64_json": BASE64.encode(&[4, 5]), "revised_prompt": "a cat"},
                    ]
                }
            }]
        });
        let images = run_async(parse_chat_image_response(&response)).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].data, vec![4, 5]);
        assert_eq!(images[0].revised_prompt.as_deref(), Some("a cat"));
    }

    #[test]
    fn parse_chat_image_response_extracts_inline_image_url_data_url() {
        let response = json!({
            "choices": [{
                "message": {
                    "content": [
                        {"type": "text", "text": "here is your image"},
                        {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", BASE64.encode(&[7, 8, 9]))}}
                    ]
                }
            }]
        });
        let images = run_async(parse_chat_image_response(&response)).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].data, vec![7, 8, 9]);
        assert_eq!(images[0].mime, "image/png");
    }

    #[test]
    fn decode_data_url_parses_png() {
        let url = format!("data:image/png;base64,{}", BASE64.encode(&[1, 2]));
        let decoded = decode_data_url(&url).unwrap();
        assert_eq!(decoded.0, vec![1, 2]);
        assert_eq!(decoded.1, "image/png");
    }

    #[test]
    fn decode_data_url_returns_none_for_non_data_url() {
        assert!(decode_data_url("https://example.com/img.png").is_none());
    }

    #[test]
    fn generate_endpoints_validates_config() {
        let mut generate = workspace_model::ImageGenerateSettings::default();
        assert!(generate_endpoints(&generate, None).is_err());
        generate.model = "m".into();
        assert!(generate_endpoints(&generate, None).is_err()); // missing base_url
        generate.base_url = "https://api.test/v1".into();
        assert!(generate_endpoints(&generate, None).is_err()); // missing key
        let (model, base, key) =
            generate_endpoints(&generate, Some("sk-test")).unwrap();
        assert_eq!(model, "m");
        assert_eq!(base, "https://api.test/v1");
        assert_eq!(key, "sk-test");
    }

    #[test]
    fn percent_encode_query_preserves_safe_chars() {
        assert_eq!(percent_encode_query("abc-_.~123"), "abc-_.~123");
        assert_eq!(percent_encode_query("a b/c"), "a%20b%2Fc");
    }

    // Suppress unused-import warning parity; future fetch tests use Write.
    #[allow(dead_code)]
    fn _write_suppressor(_w: &mut dyn Write) {}
}
