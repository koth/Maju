use anyhow::{Context, anyhow};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::lookup_host;

pub const WEB_TOOLS_PROVIDER_BRAVE: &str = "brave";
pub const WEB_TOOLS_PROVIDER_TAVILY: &str = "tavily";

const BRAVE_WEB_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const TAVILY_SEARCH_URL: &str = "https://api.tavily.com/search";
const DEFAULT_SEARCH_LIMIT: usize = 5;
const MAX_SEARCH_LIMIT: usize = 10;
const DEFAULT_FETCH_LENGTH: usize = 12_000;
const MAX_FETCH_LENGTH: usize = 50_000;
const MAX_FETCH_BYTES: u64 = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone)]
pub struct WebToolsConfig {
    pub provider: String,
    pub brave_api_key: String,
    pub brave_search_url: String,
    pub tavily_api_key: String,
    pub tavily_search_url: String,
    pub allow_private_network: bool,
}

impl WebToolsConfig {
    pub fn brave(api_key: impl Into<String>) -> Self {
        Self {
            provider: WEB_TOOLS_PROVIDER_BRAVE.into(),
            brave_api_key: api_key.into(),
            brave_search_url: BRAVE_WEB_SEARCH_URL.into(),
            tavily_api_key: String::new(),
            tavily_search_url: TAVILY_SEARCH_URL.into(),
            allow_private_network: false,
        }
    }

    pub fn tavily(api_key: impl Into<String>) -> Self {
        Self {
            provider: WEB_TOOLS_PROVIDER_TAVILY.into(),
            brave_api_key: String::new(),
            brave_search_url: BRAVE_WEB_SEARCH_URL.into(),
            tavily_api_key: api_key.into(),
            tavily_search_url: TAVILY_SEARCH_URL.into(),
            allow_private_network: false,
        }
    }

    pub fn for_provider(
        provider: impl AsRef<str>,
        api_key: impl Into<String>,
    ) -> anyhow::Result<Self> {
        match provider.as_ref().trim().to_ascii_lowercase().as_str() {
            WEB_TOOLS_PROVIDER_BRAVE => Ok(Self::brave(api_key)),
            WEB_TOOLS_PROVIDER_TAVILY => Ok(Self::tavily(api_key)),
            other => Err(anyhow!("Unsupported web search provider: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchRequest {
    pub query: String,
    #[serde(default)]
    pub count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchResponse {
    pub query: String,
    pub results: Vec<WebSearchResult>,
    pub limited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    #[serde(default)]
    pub page_age: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebFetchRequest {
    pub url: String,
    #[serde(default)]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub start_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebFetchResponse {
    pub url: String,
    pub final_url: String,
    pub title: Option<String>,
    pub content_format: String,
    pub content: String,
    pub start_index: usize,
    pub next_start_index: Option<usize>,
    pub total_length: usize,
    pub limited: bool,
}

#[derive(Debug)]
pub enum WebToolsError {
    InvalidRequest(String),
    NetworkPolicy(String),
    Provider(String),
    Fetch(String),
}

impl fmt::Display for WebToolsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message)
            | Self::NetworkPolicy(message)
            | Self::Provider(message)
            | Self::Fetch(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for WebToolsError {}

type WebToolsResult<T> = Result<T, WebToolsError>;

#[derive(Clone)]
pub struct WebToolsService {
    config: WebToolsConfig,
    client: Client,
    cache: Arc<Mutex<HashMap<String, CachedEntry>>>,
}

#[derive(Clone)]
struct CachedEntry {
    created_at: Instant,
    value: CachedValue,
}

#[derive(Clone)]
enum CachedValue {
    Search(WebSearchResponse),
    Fetch(WebFetchResponse),
}

impl WebToolsService {
    pub fn new(config: WebToolsConfig) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(Policy::none())
            .build()
            .context("failed to build web tools HTTP client")?;
        Ok(Self {
            config,
            client,
            cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn search(&self, request: WebSearchRequest) -> WebToolsResult<WebSearchResponse> {
        let query = request.query.trim();
        if query.is_empty() {
            return Err(WebToolsError::InvalidRequest(
                "web_search query cannot be empty".into(),
            ));
        }
        let requested = request.count.unwrap_or(DEFAULT_SEARCH_LIMIT);
        let count = requested.clamp(1, MAX_SEARCH_LIMIT);
        let limited = requested > MAX_SEARCH_LIMIT;
        let cache_key = format!(
            "search:{}:{}:{count}",
            self.config.provider,
            query.to_ascii_lowercase()
        );
        if let Some(CachedValue::Search(response)) = self.cache_get(&cache_key) {
            return Ok(response);
        }

        let mut results = match self.config.provider.as_str() {
            WEB_TOOLS_PROVIDER_BRAVE => self.search_brave(query, count).await?,
            WEB_TOOLS_PROVIDER_TAVILY => self.search_tavily(query, count).await?,
            unsupported => {
                return Err(WebToolsError::Provider(format!(
                    "Unsupported web search provider: {unsupported}"
                )));
            }
        };
        let response = WebSearchResponse {
            query: query.to_string(),
            limited: limited || results.len() == count,
            results: std::mem::take(&mut results),
        };
        self.cache_insert(cache_key, CachedValue::Search(response.clone()));
        Ok(response)
    }

    async fn search_brave(
        &self,
        query: &str,
        count: usize,
    ) -> WebToolsResult<Vec<WebSearchResult>> {
        if self.config.brave_api_key.trim().is_empty() {
            return Err(WebToolsError::Provider(
                "Brave Search API key is not configured".into(),
            ));
        }

        let mut url = Url::parse(&self.config.brave_search_url)
            .map_err(|error| WebToolsError::Provider(format!("Invalid Brave endpoint: {error}")))?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("count", &count.to_string());

        let response = self
            .client
            .get(url)
            .header("X-Subscription-Token", self.config.brave_api_key.trim())
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| WebToolsError::Provider(format!("Search provider failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(WebToolsError::Provider(format!(
                "Search provider returned {status}"
            )));
        }
        let payload: BraveSearchResponse = response.json().await.map_err(|error| {
            WebToolsError::Provider(format!("Invalid search response: {error}"))
        })?;
        Ok(payload
            .web
            .map(|web| web.results)
            .unwrap_or_default()
            .into_iter()
            .filter_map(WebSearchResult::from_brave)
            .take(count)
            .collect())
    }

    async fn search_tavily(
        &self,
        query: &str,
        count: usize,
    ) -> WebToolsResult<Vec<WebSearchResult>> {
        if self.config.tavily_api_key.trim().is_empty() {
            return Err(WebToolsError::Provider(
                "Tavily API key is not configured".into(),
            ));
        }

        let url = Url::parse(&self.config.tavily_search_url).map_err(|error| {
            WebToolsError::Provider(format!("Invalid Tavily endpoint: {error}"))
        })?;
        let response = self
            .client
            .post(url)
            .bearer_auth(self.config.tavily_api_key.trim())
            .header(ACCEPT, "application/json")
            .json(&TavilySearchRequest {
                query,
                max_results: count,
                search_depth: "basic",
                include_answer: false,
                include_raw_content: false,
            })
            .send()
            .await
            .map_err(|error| WebToolsError::Provider(format!("Search provider failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(WebToolsError::Provider(format!(
                "Search provider returned {status}"
            )));
        }
        let payload: TavilySearchResponse = response.json().await.map_err(|error| {
            WebToolsError::Provider(format!("Invalid search response: {error}"))
        })?;
        Ok(payload
            .results
            .into_iter()
            .filter_map(WebSearchResult::from_tavily)
            .take(count)
            .collect())
    }

    pub async fn fetch(&self, request: WebFetchRequest) -> WebToolsResult<WebFetchResponse> {
        let url = Url::parse(request.url.trim())
            .map_err(|error| WebToolsError::InvalidRequest(format!("Invalid URL: {error}")))?;
        let start_index = request.start_index.unwrap_or(0);
        let max_length = request
            .max_length
            .unwrap_or(DEFAULT_FETCH_LENGTH)
            .clamp(1, MAX_FETCH_LENGTH);
        let cache_key = format!("fetch:{}:{start_index}:{max_length}", url.as_str());
        if let Some(CachedValue::Fetch(response)) = self.cache_get(&cache_key) {
            return Ok(response);
        }

        self.validate_public_url(&url).await?;
        let (final_url, content_type, body) = self.fetch_bytes(url.clone()).await?;
        self.validate_public_url(&final_url).await?;
        let (title, content_format, extracted) = extract_content(&body, content_type.as_deref());
        let total_length = extracted.len();
        let safe_start = previous_char_boundary(&extracted, start_index.min(total_length));
        let end = previous_char_boundary(
            &extracted,
            safe_start.saturating_add(max_length).min(total_length),
        );
        let content = extracted[safe_start..end].to_string();
        let next_start_index = (end < total_length).then_some(end);
        let response = WebFetchResponse {
            url: url.to_string(),
            final_url: final_url.to_string(),
            title,
            content_format,
            content,
            start_index: safe_start,
            next_start_index,
            total_length,
            limited: next_start_index.is_some(),
        };
        self.cache_insert(cache_key, CachedValue::Fetch(response.clone()));
        Ok(response)
    }

    async fn fetch_bytes(&self, mut url: Url) -> WebToolsResult<(Url, Option<String>, Vec<u8>)> {
        for _ in 0..=MAX_REDIRECTS {
            let response = self
                .client
                .get(url.clone())
                .header(ACCEPT, "text/html, text/plain, application/xhtml+xml, */*")
                .send()
                .await
                .map_err(|error| WebToolsError::Fetch(format!("Fetch failed: {error}")))?;
            let status = response.status();
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        WebToolsError::Fetch("Redirect response missing Location header".into())
                    })?;
                let next = url
                    .join(location)
                    .map_err(|error| WebToolsError::Fetch(format!("Invalid redirect: {error}")))?;
                self.validate_public_url(&next).await?;
                url = next;
                continue;
            }
            if !status.is_success() {
                return Err(WebToolsError::Fetch(format!("Fetch returned {status}")));
            }
            if response
                .content_length()
                .is_some_and(|length| length > MAX_FETCH_BYTES)
            {
                return Err(WebToolsError::Fetch(format!(
                    "Response exceeds {} bytes",
                    MAX_FETCH_BYTES
                )));
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let bytes = response.bytes().await.map_err(|error| {
                WebToolsError::Fetch(format!("Failed to read response: {error}"))
            })?;
            if bytes.len() as u64 > MAX_FETCH_BYTES {
                return Err(WebToolsError::Fetch(format!(
                    "Response exceeds {} bytes",
                    MAX_FETCH_BYTES
                )));
            }
            return Ok((url, content_type, bytes.to_vec()));
        }
        Err(WebToolsError::Fetch("Too many redirects".into()))
    }

    async fn validate_public_url(&self, url: &Url) -> WebToolsResult<()> {
        match url.scheme() {
            "http" | "https" => {}
            _ => {
                return Err(WebToolsError::NetworkPolicy(
                    "Only public HTTP and HTTPS URLs are supported".into(),
                ));
            }
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(WebToolsError::NetworkPolicy(
                "Credential-bearing URLs are not supported".into(),
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| WebToolsError::InvalidRequest("URL is missing a host".into()))?;
        if is_local_hostname(host) {
            return self.block_or_allow_private("Localhost URLs are blocked");
        }
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_blocked_ip(ip) {
                return self.block_or_allow_private("Private network URLs are blocked");
            }
            return Ok(());
        }

        let port = url.port_or_known_default().unwrap_or(80);
        let addrs = lookup_host((host, port)).await.map_err(|error| {
            WebToolsError::NetworkPolicy(format!("Failed to resolve host {host}: {error}"))
        })?;
        for addr in addrs {
            if is_blocked_socket(addr) {
                return self.block_or_allow_private("Private network URLs are blocked");
            }
        }
        Ok(())
    }

    fn block_or_allow_private(&self, message: &str) -> WebToolsResult<()> {
        if self.config.allow_private_network {
            Ok(())
        } else {
            Err(WebToolsError::NetworkPolicy(message.into()))
        }
    }

    fn cache_get(&self, key: &str) -> Option<CachedValue> {
        let mut cache = self.cache.lock().ok()?;
        let entry = cache.get(key)?;
        if entry.created_at.elapsed() > CACHE_TTL {
            cache.remove(key);
            return None;
        }
        Some(entry.value.clone())
    }

    fn cache_insert(&self, key: String, value: CachedValue) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                key,
                CachedEntry {
                    created_at: Instant::now(),
                    value,
                },
            );
        }
    }
}

#[derive(Debug, Deserialize)]
struct BraveSearchResponse {
    web: Option<BraveWebResults>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResults {
    #[serde(default)]
    results: Vec<BraveWebResult>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResult {
    title: Option<String>,
    url: Option<String>,
    description: Option<String>,
    #[serde(default)]
    age: Option<String>,
    #[serde(default)]
    page_age: Option<String>,
}

#[derive(Debug, Serialize)]
struct TavilySearchRequest<'a> {
    query: &'a str,
    max_results: usize,
    search_depth: &'static str,
    include_answer: bool,
    include_raw_content: bool,
}

#[derive(Debug, Deserialize)]
struct TavilySearchResponse {
    #[serde(default)]
    results: Vec<TavilySearchResult>,
}

#[derive(Debug, Deserialize)]
struct TavilySearchResult {
    title: Option<String>,
    url: Option<String>,
    content: Option<String>,
    #[serde(default)]
    published_date: Option<String>,
}

impl WebSearchResult {
    fn from_brave(value: BraveWebResult) -> Option<Self> {
        let title = value.title?.trim().to_string();
        let url = value.url?.trim().to_string();
        Some(Self {
            title,
            url,
            snippet: value.description.unwrap_or_default(),
            page_age: value.page_age.or(value.age),
        })
    }

    fn from_tavily(value: TavilySearchResult) -> Option<Self> {
        let title = value.title?.trim().to_string();
        let url = value.url?.trim().to_string();
        Some(Self {
            title,
            url,
            snippet: value.content.unwrap_or_default(),
            page_age: value.published_date,
        })
    }
}

fn is_local_hostname(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost" || host.ends_with(".localhost")
}

fn is_blocked_socket(addr: SocketAddr) -> bool {
    is_blocked_ip(addr.ip())
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 224
                || ip == std::net::Ipv4Addr::new(169, 254, 169, 254)
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
        }
    }
}

fn extract_content(bytes: &[u8], content_type: Option<&str>) -> (Option<String>, String, String) {
    let raw = String::from_utf8_lossy(bytes).to_string();
    if content_type
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("html")
        || raw.to_ascii_lowercase().contains("<html")
    {
        let title = extract_title(&raw);
        let without_head = remove_block_tag(&raw, "head");
        let without_scripts = remove_block_tag(&remove_block_tag(&without_head, "script"), "style");
        let text = normalize_whitespace(&decode_basic_entities(&strip_tags(&without_scripts)));
        return (title, "markdown".into(), text);
    }
    (None, "text".into(), raw)
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = lower[start..].find('>')? + start + 1;
    let close = lower[open_end..].find("</title>")? + open_end;
    let title = decode_basic_entities(&html[open_end..close]);
    let title = normalize_whitespace(&title);
    (!title.is_empty()).then_some(title)
}

fn remove_block_tag(input: &str, tag: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(start_rel) = lower[cursor..].find(&open) {
        let start = cursor + start_rel;
        output.push_str(&input[cursor..start]);
        let Some(end_rel) = lower[start..].find(&close) else {
            cursor = input.len();
            break;
        };
        cursor = start + end_rel + close.len();
    }
    output.push_str(&input[cursor..]);
    output
}

fn strip_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => {
                in_tag = true;
                output.push(' ');
            }
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn decode_basic_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn previous_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub fn service_from_brave_key(api_key: impl Into<String>) -> anyhow::Result<WebToolsService> {
    WebToolsService::new(WebToolsConfig::brave(api_key))
}

pub fn service_from_tavily_key(api_key: impl Into<String>) -> anyhow::Result<WebToolsService> {
    WebToolsService::new(WebToolsConfig::tavily(api_key))
}

pub fn error_to_json(error: &WebToolsError) -> serde_json::Value {
    let kind = match error {
        WebToolsError::InvalidRequest(_) => "invalid_request",
        WebToolsError::NetworkPolicy(_) => "network_policy",
        WebToolsError::Provider(_) => "provider",
        WebToolsError::Fetch(_) => "fetch",
    };
    serde_json::json!({
        "error": {
            "kind": kind,
            "message": error.to_string(),
        }
    })
}

pub fn response_to_value<T: Serialize>(value: &T) -> anyhow::Result<serde_json::Value> {
    serde_json::to_value(value)
        .map_err(|error| anyhow!("failed to serialize web tool result: {error}"))
}

pub fn web_tool_headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(token) {
        headers.insert("x-kodex-web-tools-token", value);
    }
    headers
}

pub fn is_success_status(status: StatusCode) -> bool {
    status.is_success()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn run_async<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn local_http_server(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{}", addr.port())
    }

    fn test_service() -> WebToolsService {
        let mut config = WebToolsConfig::brave("secret");
        config.allow_private_network = true;
        WebToolsService::new(config).unwrap()
    }

    #[test]
    fn extracts_html_title_and_text() {
        let (title, format, text) = extract_content(
            br#"<html><head><title>Example &amp; Docs</title><style>.x{}</style></head><body><h1>Hello</h1><script>bad()</script><p>World</p></body></html>"#,
            Some("text/html"),
        );

        assert_eq!(title.as_deref(), Some("Example & Docs"));
        assert_eq!(format, "markdown");
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn blocks_private_url_by_default() {
        let service = WebToolsService::new(WebToolsConfig::brave("secret")).unwrap();
        let url = Url::parse("http://127.0.0.1:1234").unwrap();

        let error = run_async(service.validate_public_url(&url)).unwrap_err();

        assert!(matches!(error, WebToolsError::NetworkPolicy(_)));
    }

    #[test]
    fn rejects_credential_url() {
        let service = test_service();
        let url = Url::parse("https://user:pass@example.com").unwrap();

        let error = run_async(service.validate_public_url(&url)).unwrap_err();

        assert!(error.to_string().contains("Credential-bearing"));
    }

    #[test]
    fn fetches_and_chunks_content() {
        let url = local_http_server(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 79\r\n\r\n<html><head><title>T</title></head><body><p>alpha beta gamma delta</p></body></html>",
        );
        let service = test_service();

        let response = run_async(service.fetch(WebFetchRequest {
            url,
            max_length: Some(10),
            start_index: Some(0),
        }))
        .unwrap();

        assert_eq!(response.title.as_deref(), Some("T"));
        assert_eq!(response.content, "alpha beta");
        assert_eq!(response.next_start_index, Some(10));
        assert!(response.limited);
    }

    #[test]
    fn brave_search_maps_results_and_limits_count() {
        let body = r#"{"web":{"results":[{"title":"One","url":"https://example.com/1","description":"A","age":"1 day"},{"title":"Two","url":"https://example.com/2","description":"B"}]}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let url = local_http_server(Box::leak(response.into_boxed_str()));
        let mut config = WebToolsConfig::brave("secret");
        config.brave_search_url = url;
        let service = WebToolsService::new(config).unwrap();

        let response = run_async(service.search(WebSearchRequest {
            query: "rust".into(),
            count: Some(MAX_SEARCH_LIMIT + 5),
        }))
        .unwrap();

        assert_eq!(response.results.len(), 2);
        assert!(response.limited);
        assert_eq!(response.results[0].title, "One");
        assert_eq!(response.results[0].page_age.as_deref(), Some("1 day"));
    }

    #[test]
    fn tavily_search_maps_results() {
        let body = r#"{"results":[{"title":"Tavily One","url":"https://example.com/t1","content":"Snippet one","published_date":"2026-01-02"},{"title":"Tavily Two","url":"https://example.com/t2","content":"Snippet two"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let url = local_http_server(Box::leak(response.into_boxed_str()));
        let mut config = WebToolsConfig::tavily("secret");
        config.tavily_search_url = url;
        let service = WebToolsService::new(config).unwrap();

        let response = run_async(service.search(WebSearchRequest {
            query: "rust web search".into(),
            count: Some(2),
        }))
        .unwrap();

        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[0].title, "Tavily One");
        assert_eq!(response.results[0].snippet, "Snippet one");
        assert_eq!(response.results[0].page_age.as_deref(), Some("2026-01-02"));
    }
}
