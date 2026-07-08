use serde_json::{Value, json};
fn base_url() -> String {
    std::env::var("PROXY_URL").unwrap_or_else(|_| "http://127.0.0.1:17856".to_string())
}
fn model() -> String {
    std::env::var("PROXY_MODEL").unwrap_or_else(|_| "claude-sonnet-5".to_string())
}
async fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap()
}
async fn post_json(c: &reqwest::Client, path: &str, body: &Value) -> Value {
    let resp = c
        .post(format!("{}{}", base_url(), path))
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let text = resp.text().await.expect("body");
    if !status.is_success() {
        panic!("{path} {status}: {text}");
    }
    serde_json::from_str(&text).expect("json")
}
#[tokio::test]
#[ignore = "requires real CodeBuddy CLI; run with CODEBUDDY_E2E=1"]
async fn test_models() {
    if std::env::var("CODEBUDDY_E2E").as_deref() != Ok("1") {
        return;
    }
    let c = client().await;
    let resp = c.get(format!("{}/v1/models", base_url())).send().await.unwrap();
    let data: Value = resp.json().await.unwrap();
    assert_eq!(data["object"], "list");
    assert!(data["data"].as_array().unwrap().len() > 0);
}
#[tokio::test]
#[ignore = "requires real CodeBuddy CLI; run with CODEBUDDY_E2E=1"]
async fn test_non_streaming() {
    if std::env::var("CODEBUDDY_E2E").as_deref() != Ok("1") {
        return;
    }
    let c = client().await;
    let data = post_json(
        &c,
        "/v1/chat/completions",
        &json!({
            "model": model(),
            "messages": [{"role": "user", "content": "Reply with exactly the word: PONG"}],
            "stream": false,
        }),
    )
    .await;
    assert_eq!(data["object"], "chat.completion");
    let text = data["choices"][0]["message"]["content"].as_str().unwrap_or("");
    assert!(text.contains("PONG"), "content: {text}");
}
#[tokio::test]
#[ignore = "requires real CodeBuddy CLI; run with CODEBUDDY_E2E=1"]
async fn test_streaming() {
    if std::env::var("CODEBUDDY_E2E").as_deref() != Ok("1") {
        return;
    }
    let c = client().await;
    let resp = c
        .post(format!("{}/v1/chat/completions", base_url()))
        .header("content-type", "application/json")
        .json(&json!({
            "model": model(),
            "messages": [{"role": "user", "content": "Count from 1 to 5, one number per line, nothing else."}],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    let text = resp.text().await.unwrap();
    assert!(text.contains("data: "), "no SSE frames");
    assert!(text.contains("[DONE]"), "no DONE");
}
#[tokio::test]
#[ignore = "requires real CodeBuddy CLI; run with CODEBUDDY_E2E=1"]
async fn test_tool_call_passthrough() {
    if std::env::var("CODEBUDDY_E2E").as_deref() != Ok("1") {
        return;
    }
    let c = client().await;
    let data = post_json(
        &c,
        "/v1/chat/completions",
        &json!({
            "model": model(),
            "messages": [{"role": "user", "content": "What's the weather in Tokyo? Use the get_weather tool."}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get current weather for a location",
                    "parameters": {
                        "type": "object",
                        "properties": {"location": {"type": "string", "description": "City name"}},
                        "required": ["location"],
                    },
                },
            }],
            "stream": false,
        }),
    )
    .await;
    let choice = &data["choices"][0];
    assert_eq!(choice["finish_reason"], "tool_calls");
    let tc = &choice["message"]["tool_calls"][0];
    assert_eq!(tc["function"]["name"], "get_weather");
}
#[tokio::test]
#[ignore = "requires real CodeBuddy CLI; run with CODEBUDDY_E2E=1"]
async fn test_session_reuse() {
    if std::env::var("CODEBUDDY_E2E").as_deref() != Ok("1") {
        return;
    }
    let c = client().await;
    let sid = format!("e2e-reuse-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    let r1 = c
        .post(format!("{}/v1/chat/completions", base_url()))
        .header("content-type", "application/json")
        .header("x-session-id", &sid)
        .json(&json!({
            "model": model(),
            "messages": [{"role": "user", "content": "Remember the secret word: BANANA. Just reply \"OK\"."}],
            "stream": false,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.headers().get("x-session-id").unwrap(), &sid);
    let j1: Value = r1.json().await.unwrap();
    assert!(j1["choices"][0]["message"]["content"].as_str().unwrap_or("").contains("OK"));
    let r2 = c
        .post(format!("{}/v1/chat/completions", base_url()))
        .header("content-type", "application/json")
        .header("x-session-id", &sid)
        .json(&json!({
            "model": model(),
            "messages": [{"role": "user", "content": "What was the secret word I told you? Reply with only the word."}],
            "stream": false,
        }))
        .send()
        .await
        .unwrap();
    let j2: Value = r2.json().await.unwrap();
    let ans = j2["choices"][0]["message"]["content"].as_str().unwrap_or("").to_uppercase();
    assert!(ans.contains("BANANA"), "recalled: {ans}");
    let _ = c.delete(format!("{}/v1/sessions/{}", base_url(), sid)).send().await;
}
