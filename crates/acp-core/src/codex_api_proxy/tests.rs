use super::*;

#[test]
fn converts_responses_request_to_chat_payload() {
    let payload = json!({
        "model": "glm-5.1",
        "instructions": "base instructions",
        "input": [
            {
                "type": "message",
                "role": "developer",
                "content": [{ "type": "input_text", "text": "dev instructions" }]
            },
            {
                "role": "user",
                "content": [{ "type": "input_text", "text": "hello" }]
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "hi" }],
                "reasoning_content": "previous thinking"
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "list_files",
                "arguments": "{\"path\":\".\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "file list"
            }
        ],
        "tools": [{
            "type": "function",
            "name": "list_files",
            "description": "List files",
            "parameters": { "type": "object", "properties": {} }
        }],
        "stream": true
    });

    let chat = responses_payload_to_chat_payload(payload, "timiai").unwrap();

    assert_eq!(chat["model"], "glm-5.1");
    assert_eq!(chat["stream"], true);
    assert_eq!(chat["messages"][0]["role"], "system");
    assert_eq!(chat["messages"][0]["content"], "base instructions");
    assert_eq!(chat["messages"][1]["role"], "system");
    assert_eq!(chat["messages"][1]["content"], "dev instructions");
    assert_eq!(chat["messages"][2]["role"], "user");
    assert_eq!(chat["messages"][2]["content"], "hello");
    assert_eq!(chat["messages"][3]["role"], "assistant");
    assert_eq!(chat["messages"][3]["content"], "hi");
    assert_eq!(
        chat["messages"][3]["reasoning_content"],
        "previous thinking"
    );
    assert_eq!(chat["messages"][4]["tool_calls"][0]["id"], "call_1");
    assert_eq!(chat["messages"][5]["role"], "tool");
    assert_eq!(chat["messages"][5]["tool_call_id"], "call_1");
    assert_eq!(chat["tools"][0]["function"]["name"], "list_files");
    assert_eq!(chat["tool_choice"], "auto");
}

#[test]
fn converts_responses_image_input_to_chat_image_content() {
    let payload = json!({
        "model": "glm-5.1",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [
                { "type": "input_text", "text": "what is this?" },
                { "type": "input_image", "image_url": "data:image/png;base64,aW1n" }
            ]
        }]
    });

    let chat = responses_payload_to_chat_payload(payload, "kimi_code").unwrap();

    assert_eq!(chat["messages"][0]["role"], "user");
    assert_eq!(chat["messages"][0]["content"][0]["type"], "text");
    assert_eq!(chat["messages"][0]["content"][0]["text"], "what is this?");
    assert_eq!(chat["messages"][0]["content"][1]["type"], "image_url");
    assert_eq!(
        chat["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,aW1n"
    );
}

#[test]
fn converts_apply_patch_custom_tool_to_chat_function_tool() {
    let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
    let payload = json!({
        "model": "gpt-5.5",
        "input": [
            { "role": "user", "content": "edit" },
            {
                "type": "custom_tool_call",
                "call_id": "call_patch",
                "name": "apply_patch",
                "input": patch
            },
            {
                "type": "custom_tool_call_output",
                "call_id": "call_patch",
                "output": "Done"
            }
        ],
        "tools": [{
            "type": "custom",
            "name": "apply_patch",
            "description": "Use the `apply_patch` tool to edit files.",
            "format": { "type": "grammar", "syntax": "lark", "definition": "start: begin_patch" }
        }]
    });

    let chat = responses_payload_to_chat_payload(payload, "timiai").unwrap();

    assert_eq!(
        chat["tools"][0]["function"]["parameters"]["properties"]["patch"]["type"],
        "string"
    );
    assert_eq!(
        chat["messages"][1]["tool_calls"][0]["function"]["name"],
        "apply_patch"
    );
    let arguments = chat["messages"][1]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap();
    let arguments: Value = serde_json::from_str(arguments).unwrap();
    assert_eq!(arguments["patch"], patch);
    assert_eq!(chat["messages"][2]["role"], "tool");
    assert_eq!(chat["messages"][2]["tool_call_id"], "call_patch");
}

#[test]
fn kimi_code_expands_apply_patch_tool_to_claude_style_edit_tools() {
    let payload = json!({
        "model": "kimi-for-coding",
        "input": [{ "role": "user", "content": "edit" }],
        "tools": [{
            "type": "custom",
            "name": "apply_patch",
            "description": "Use the `apply_patch` tool to edit files.",
            "format": { "type": "grammar", "syntax": "lark", "definition": "start: begin_patch" }
        }]
    });

    let chat = responses_payload_to_chat_payload(payload, "kimi_code").unwrap();
    let tool_names = chat["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        tool_names,
        vec!["Edit", "MultiEdit", "Write", "apply_patch"]
    );

    let anthropic = chat_payload_to_anthropic_payload(chat);
    let anthropic_tool_names = anthropic["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        anthropic_tool_names,
        vec!["Edit", "MultiEdit", "Write", "apply_patch"]
    );
}

#[test]
fn non_gpt_models_expand_apply_patch_tool_and_get_bridge_instructions() {
    let payload = json!({
        "model": "deepseek-v4-pro",
        "instructions": "base instructions",
        "input": [{ "role": "user", "content": "edit" }],
        "tools": [{
            "type": "custom",
            "name": "apply_patch",
            "description": "Use the `apply_patch` tool to edit files.",
            "format": { "type": "grammar", "syntax": "lark", "definition": "start: begin_patch" }
        }]
    });

    let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();
    let tool_names = chat["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        tool_names,
        vec!["Edit", "MultiEdit", "Write", "apply_patch"]
    );
    assert_eq!(chat["messages"][0]["content"], "base instructions");
    assert_eq!(
        chat["messages"][1]["content"],
        NON_GPT_EDIT_BRIDGE_INSTRUCTIONS
    );
    assert_eq!(chat["messages"][2]["role"], "user");

    let anthropic = chat_payload_to_anthropic_payload(chat);
    let system = anthropic["system"].as_str().unwrap();
    assert!(system.contains("base instructions"));
    assert!(system.contains(NON_GPT_EDIT_BRIDGE_INSTRUCTIONS));
}

#[test]
fn gpt_models_keep_apply_patch_as_the_only_edit_tool() {
    let payload = json!({
        "model": "openai/gpt-5.5",
        "input": [{ "role": "user", "content": "edit" }],
        "tools": [{
            "type": "custom",
            "name": "apply_patch",
            "description": "Use the `apply_patch` tool to edit files.",
            "format": { "type": "grammar", "syntax": "lark", "definition": "start: begin_patch" }
        }]
    });

    let chat = responses_payload_to_chat_payload(payload, "timiai").unwrap();
    let tool_names = chat["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(tool_names, vec!["apply_patch"]);
    assert_eq!(chat["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn gpt_models_get_shell_tool_instructions() {
    let payload = json!({
        "model": "openai/gpt-5.5",
        "instructions": "base instructions",
        "input": [{ "role": "user", "content": "run validation" }],
        "tools": [{
            "type": "function",
            "name": "bash",
            "description": "Run a shell command in the project workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "cmd": { "type": "string" }
                },
                "required": ["cmd"]
            }
        }]
    });

    let chat = responses_payload_to_chat_payload(payload, "timiai").unwrap();

    assert_eq!(chat["messages"][0]["content"], "base instructions");
    assert_eq!(chat["messages"][1]["role"], "user");
    assert_eq!(chat["tools"][0]["function"]["name"], "bash");
    assert!(
        chat["tools"][0]["function"]["description"]
            .as_str()
            .unwrap()
            .contains(SHELL_TOOL_INSTRUCTIONS)
    );
}

#[test]
fn converts_claude_style_edit_tool_call_to_apply_patch_custom_call() {
    let chat = json!({
        "id": "chatcmpl_1",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_edit",
                    "type": "function",
                    "function": {
                        "name": "Edit",
                        "arguments": serde_json::to_string(&json!({
                            "file_path": "src/lib.rs",
                            "old_string": "old",
                            "new_string": "new"
                        })).unwrap()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });

    let response = chat_response_to_responses_response(chat).unwrap();

    assert_eq!(response["output"][0]["type"], "custom_tool_call");
    assert_eq!(response["output"][0]["name"], "apply_patch");
    let input = response["output"][0]["input"].as_str().unwrap();
    assert!(input.contains("*** Update File: src/lib.rs"));
    assert!(input.contains("-old"));
    assert!(input.contains("+new"));
}

#[test]
fn converts_claude_style_multi_edit_and_write_to_apply_patch() {
    let multi_edit = claude_edit_tool_arguments_to_apply_patch(
        "MultiEdit",
        &json!({
            "file_path": "src/lib.rs",
            "edits": [
                { "old_string": "one", "new_string": "two" },
                { "old_string": "three", "new_string": "four" }
            ]
        })
        .to_string(),
    )
    .unwrap();
    assert!(multi_edit.contains("*** Update File: src/lib.rs"));
    assert!(multi_edit.contains("-one"));
    assert!(multi_edit.contains("+two"));
    assert!(multi_edit.contains("-three"));
    assert!(multi_edit.contains("+four"));

    let write = claude_edit_tool_arguments_to_apply_patch(
        "Write",
        &json!({
            "file_path": "src/new.rs",
            "content": "pub fn probe() {}\n"
        })
        .to_string(),
    )
    .unwrap();
    assert!(write.contains("*** Add File: src/new.rs"));
    assert!(write.contains("+pub fn probe() {}"));
}

#[test]
fn converts_anthropic_edit_tool_use_to_apply_patch_custom_call() {
    let anthropic = json!({
        "id": "msg_1",
        "content": [{
            "type": "tool_use",
            "id": "call_edit",
            "name": "Edit",
            "input": {
                "file_path": "src/lib.rs",
                "old_string": "old",
                "new_string": "new"
            }
        }]
    });

    let response = anthropic_response_to_responses_response(anthropic);

    assert_eq!(response["output"][0]["type"], "custom_tool_call");
    assert_eq!(response["output"][0]["call_id"], "call_edit");
    assert_eq!(response["output"][0]["name"], "apply_patch");
    assert!(
        response["output"][0]["input"]
            .as_str()
            .unwrap()
            .contains("*** Update File: src/lib.rs")
    );
}

#[test]
fn converts_streaming_edit_tool_call_to_apply_patch_custom_call() {
    let arguments = serde_json::to_string(&json!({
        "file_path": "src/lib.rs",
        "old_string": "old",
        "new_string": "new"
    }))
    .unwrap();
    let sse = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "id": "chatcmpl_1",
            "model": "kimi-for-coding",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_edit",
                        "type": "function",
                        "function": {
                            "name": "Edit",
                            "arguments": arguments
                        }
                    }]
                },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl_1",
            "model": "kimi-for-coding",
            "choices": [{
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        })
    );

    let converted = chat_sse_to_responses_sse(sse.as_bytes());
    let converted = String::from_utf8(converted).unwrap();

    assert!(converted.contains(r#""type":"custom_tool_call""#));
    assert!(converted.contains(r#""name":"apply_patch""#));
    assert!(converted.contains("*** Update File: src/lib.rs"));
}

#[test]
fn deepseek_requests_preserve_upstream_streaming() {
    let payload = json!({
        "model": "deepseek-v4-pro",
        "input": "hello",
        "stream": true
    });

    let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

    assert_eq!(chat["stream"], true);
}

#[test]
fn unsupported_provider_aliases_fall_back_to_timiai() {
    assert_eq!(normalize_proxy_provider("unsupported"), "timiai");
    assert_eq!(normalize_proxy_provider("legacy-gateway"), "timiai");
}

#[test]
fn timiai_provider_uses_chat_completions_for_codex_and_native_messages() {
    assert_eq!(normalize_proxy_provider("timi-ai"), "timiai");
    assert_eq!(
        upstream_chat_completions_url("timiai"),
        TIMIAI_CHAT_COMPLETIONS_URL
    );
    assert_eq!(upstream_messages_url("timiai"), TIMIAI_MESSAGES_URL);
    assert!(is_claude_family_model("claude-sonnet-4.6"));
    assert!(is_claude_family_model("anthropic/claude-sonnet-4.6"));
    assert!(!is_claude_family_model("gpt-5.5"));
    assert_eq!(
        proxy_provider_for_model("deepseek-v4-pro", "timiai", &BTreeMap::new()),
        "deepseek"
    );
}

#[test]
fn routes_anthropic_messages_to_chat_completions_for_non_anthropic_models() {
    assert!(!should_bridge_anthropic_messages_to_chat_completions(
        "commandcode",
        "claude-sonnet-4-6"
    ));
    assert!(should_bridge_anthropic_messages_to_chat_completions(
        "commandcode",
        "Qwen/Qwen3.7-Max"
    ));
    assert!(should_bridge_anthropic_messages_to_chat_completions(
        "commandcode",
        "MiniMaxAI/MiniMax-M3"
    ));
    assert!(should_bridge_anthropic_messages_to_chat_completions(
        "deepseek",
        "deepseek-v4-pro"
    ));
    assert!(!should_bridge_anthropic_messages_to_chat_completions(
        "kimi_code",
        "kimi-for-coding"
    ));
    assert!(should_bridge_anthropic_messages_to_chat_completions(
        "xiaomi_mimo",
        "MiMo-V2.5-Pro"
    ));
    assert!(should_bridge_anthropic_messages_to_chat_completions(
        "timiai",
        "deepseek-v4-pro-r1"
    ));
    assert!(should_bridge_anthropic_messages_to_chat_completions(
        "timiai", "gpt-5.5"
    ));
    assert!(!should_bridge_anthropic_messages_to_chat_completions(
        "timiai",
        "claude-opus-4.8"
    ));
}

#[test]
fn model_provider_map_overrides_byok_provider_heuristics() {
    let mut map = BTreeMap::new();
    map.insert("claude-sonnet-4-6".to_string(), "commandcode".to_string());
    map.insert("custom-lab-model".to_string(), "kimi_code".to_string());

    assert_eq!(
        proxy_provider_for_model("claude-sonnet-4-6", "xiaomi_mimo", &map),
        "commandcode"
    );
    assert_eq!(
        proxy_provider_for_model("custom-lab-model", "timiai", &map),
        "kimi_code"
    );
    assert_eq!(
        proxy_provider_for_model("Qwen/Qwen3.7-Max", "xiaomi_mimo", &map),
        "commandcode"
    );
}

#[test]
fn model_provider_map_parser_keeps_first_provider_for_duplicate_models() {
    let value = json!([
        {
            "model": "deepseek-v4-pro-r1",
            "display_name": "deepseek-v4-pro-r1",
            "provider": "timiai"
        },
        {
            "model": "deepseek-v4-pro-r1",
            "display_name": "deepseek-v4-pro-r1",
            "provider": "commandcode"
        }
    ]);

    let (map, duplicate_count) = parse_model_provider_map(&value.to_string()).unwrap();

    assert_eq!(
        map.get("deepseek-v4-pro-r1").map(String::as_str),
        Some("timiai")
    );
    assert_eq!(duplicate_count, 3);
}

#[test]
fn decodes_provider_qualified_model_ids() {
    let model = decode_provider_model_id("kodex-provider/timiai/gpt-5.5").unwrap();

    assert_eq!(model.provider, "timiai");
    assert_eq!(model.model, "gpt-5.5");

    let model = decode_provider_model_id("kodex-provider/commandcode/Qwen/Qwen3.7-Max").unwrap();
    assert_eq!(model.provider, "commandcode");
    assert_eq!(model.model, "Qwen/Qwen3.7-Max");
}

#[test]
fn converts_non_claude_anthropic_request_to_timiai_responses_payload() {
    let anthropic = json!({
        "model": "gpt-5.5",
        "max_tokens": 1024,
        "stream": true,
        "system": [{
            "type": "text",
            "text": "You are helpful",
            "cache_control": { "type": "ephemeral" }
        }],
        "messages": [
            { "role": "user", "content": "hello" },
            {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "read_file",
                    "input": { "path": "README.md" }
                }]
            },
            {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "content": "file body"
                }]
            }
        ],
        "tools": [{
            "name": "read_file",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": { "path": { "type": "string" } }
            }
        }],
        "tool_choice": { "type": "auto" }
    });

    let responses = anthropic_payload_to_responses_payload(anthropic);

    assert_eq!(responses["model"], "gpt-5.5");
    assert_eq!(responses["max_output_tokens"], 1024);
    assert_eq!(responses["stream"], true);
    assert_eq!(responses["instructions"], "You are helpful");
    assert_eq!(responses["input"][0]["role"], "user");
    assert_eq!(responses["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(responses["input"][1]["type"], "function_call");
    assert_eq!(responses["input"][1]["name"], "read_file");
    assert_eq!(responses["input"][2]["type"], "function_call_output");
    assert_eq!(responses["tools"][0]["type"], "function");
    assert_eq!(responses["tools"][0]["name"], "read_file");
    assert_eq!(responses["tool_choice"], "auto");
}

#[test]
fn converts_timiai_responses_response_to_anthropic_message() {
    let responses = json!({
        "id": "resp_1",
        "model": "gpt-5.5",
        "output": [
            {
                "type": "message",
                "content": [{ "type": "output_text", "text": "checking" }]
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "read_file",
                "arguments": "{\"path\":\"README.md\"}"
            }
        ],
        "usage": { "input_tokens": 12, "output_tokens": 4 }
    });

    let anthropic = responses_response_to_anthropic_response(responses);

    assert_eq!(anthropic["id"], "resp_1");
    assert_eq!(anthropic["model"], "gpt-5.5");
    assert_eq!(anthropic["content"][0]["type"], "text");
    assert_eq!(anthropic["content"][0]["text"], "checking");
    assert_eq!(anthropic["content"][1]["type"], "tool_use");
    assert_eq!(anthropic["content"][1]["id"], "call_1");
    assert_eq!(anthropic["content"][1]["input"]["path"], "README.md");
    assert_eq!(anthropic["stop_reason"], "tool_use");
}

#[test]
fn sanitizes_timiai_responses_payload_extensions() {
    let payload = json!({
        "model": "gpt-5.5",
        "input": [
            {
                "type": "reasoning",
                "summary": [],
                "content": null
            },
            {
                "type": "message",
                "role": "user",
                "content": "hello"
            },
            {
                "type": "message",
                "role": "assistant",
                "phase": "commentary",
                "content": [{ "type": "output_text", "text": "interim note" }]
            },
            {
                "type": "message",
                "role": "assistant",
                "phase": "final_answer",
                "content": [{ "type": "output_text", "text": "final" }]
            }
        ],
        "context_management": { "strategy": "auto" },
        "reasoning": { "effort": "medium" },
        "stream": true
    });

    let sanitized = sanitize_timiai_responses_payload(payload);

    assert!(sanitized.get("context_management").is_none());
    assert!(sanitized.get("reasoning").is_none());
    assert_eq!(sanitized["model"], "gpt-5.5");
    assert_eq!(sanitized["input"].as_array().unwrap().len(), 2);
    assert_eq!(sanitized["input"][0]["type"], "message");
    assert_eq!(sanitized["input"][0]["content"], "hello");
    assert_eq!(sanitized["input"][1]["phase"], "final_answer");
    assert_eq!(sanitized["stream"], true);
}

#[test]
fn timiai_responses_payload_is_prepared_before_upstream_logging() {
    let payload = json!({
        "model": "gpt-5.5",
        "input": [
            { "type": "reasoning", "summary": [] },
            {
                "type": "message",
                "role": "assistant",
                "phase": "commentary",
                "content": [{ "type": "output_text", "text": "planning" }]
            },
            {
                "type": "message",
                "role": "assistant",
                "phase": "final_answer",
                "content": [{ "type": "output_text", "text": "done" }]
            }
        ],
        "reasoning": { "effort": "medium" }
    });

    let prepared = prepare_responses_payload_for_provider(payload, "timiai");

    assert!(prepared.get("reasoning").is_none());
    assert_eq!(prepared["input"].as_array().unwrap().len(), 1);
    assert_eq!(prepared["input"][0]["phase"], "final_answer");
    assert_eq!(prepared["input"][0]["content"][0]["text"], "done");
}

#[test]
fn sanitizes_timiai_responses_sse_reasoning_items() {
    let body = concat!(
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[]}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"content\":[]}}\n\n",
        "event: response.reasoning_text.delta\n",
        "data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"hidden\"}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"visible\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"reasoning\",\"summary\":[]},{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"visible\"}]}]}}\n\n",
        "data: [DONE]\n\n",
    );

    let mut sanitizer = TimiaiResponsesSseSanitizer::default();
    let mut sanitized = sanitizer.push_chunk(body.as_bytes());
    sanitized.extend(sanitizer.finish());
    let text = String::from_utf8(sanitized).unwrap();

    assert!(!text.contains("response.reasoning_text.delta"));
    assert!(!text.contains("\"type\":\"reasoning\""));
    assert!(text.contains("response.output_text.delta"));
    assert!(text.contains("visible"));
    assert!(text.contains("[DONE]"));
}

#[test]
fn normalizes_timiai_responses_sse_usage_cache_fields() {
    let body = concat!(
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"prompt_tokens\":120,\"prompt_tokens_details\":{\"cached_tokens\":0},\"prompt_cache_hit_tokens\":80,\"completion_tokens\":10,\"completion_tokens_details\":{\"reasoning_tokens\":3},\"total_tokens\":130}}}\n\n",
    );

    let mut sanitizer = TimiaiResponsesSseSanitizer::default();
    let mut sanitized = sanitizer.push_chunk(body.as_bytes());
    sanitized.extend(sanitizer.finish());
    let text = String::from_utf8(sanitized).unwrap();

    assert!(text.contains("\"input_tokens\":120"));
    assert!(text.contains("\"output_tokens\":10"));
    assert!(text.contains("\"total_tokens\":130"));
    assert!(text.contains("\"input_tokens_details\":{\"cached_tokens\":80}"));
    assert!(text.contains("\"output_tokens_details\":{\"reasoning_tokens\":3}"));
}

#[test]
fn converts_chat_usage_prompt_cache_hit_tokens_to_cached_input_tokens() {
    let chat = json!({
        "id": "chatcmpl_1",
        "created": 123,
        "model": "deepseek-v4-pro-r1",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "done"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 120,
            "prompt_tokens_details": {
                "cached_tokens": 0
            },
            "prompt_cache_hit_tokens": 96,
            "completion_tokens": 10,
            "total_tokens": 130
        }
    });

    let response = chat_response_to_responses_response(chat).unwrap();

    assert_eq!(response["usage"]["input_tokens"], 120);
    assert_eq!(
        response["usage"]["input_tokens_details"]["cached_tokens"],
        96
    );
    assert_eq!(response["usage"]["output_tokens"], 10);
    assert_eq!(response["usage"]["total_tokens"], 130);
}

#[test]
fn sanitizes_timiai_anthropic_messages_payload_extensions() {
    let payload = json!({
        "model": "claude-opus-4.8",
        "context_management": { "strategy": "auto" },
        "messages": [{ "role": "user", "content": "hello" }],
        "tools": [{ "name": "read_file" }]
    });

    let sanitized = sanitize_timiai_anthropic_messages_payload(payload);

    assert!(sanitized.get("context_management").is_none());
    assert_eq!(sanitized["model"], "claude-opus-4.8");
    assert_eq!(sanitized["messages"][0]["role"], "user");
    assert_eq!(sanitized["tools"][0]["name"], "read_file");
}

#[test]
fn timiai_session_id_is_reused_from_proxy_config() {
    let mut session_ids = BTreeMap::new();
    session_ids.insert("timiai".to_string(), "session-1".to_string());
    let config = CodexApiProxyConfig {
        provider: "timiai".to_string(),
        api_key: "secret".to_string(),
        api_keys: BTreeMap::new(),
        session_ids,
        model_providers: BTreeMap::new(),
    };

    assert_eq!(
        session_id_for_proxy_provider(&config, "timiai"),
        "session-1"
    );
}

#[test]
fn timiai_authorization_header_uses_saved_key_without_bearer_injection() {
    assert_eq!(
        timiai_authorization_header_value("timiai-secret"),
        "timiai-secret"
    );
    assert_eq!(
        timiai_authorization_header_value("  timiai-secret  "),
        "timiai-secret"
    );
    assert_eq!(
        timiai_authorization_header_value("Bearer timiai-secret"),
        "Bearer timiai-secret"
    );
    assert_eq!(timiai_authorization_log_state("timiai-secret"), "raw_value");
    assert_eq!(
        timiai_authorization_log_state("Bearer timiai-secret"),
        "bearer_value"
    );
}

#[test]
fn timiai_upstream_headers_include_x_api_key() {
    let request = with_timiai_headers(
        reqwest::Client::new().post("http://example.com"),
        " timiai-secret ",
        "session-1",
    )
    .build()
    .unwrap();

    assert_eq!(
        request
            .headers()
            .get("Authorization")
            .and_then(|value| value.to_str().ok()),
        Some("timiai-secret")
    );
    assert_eq!(
        request
            .headers()
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("timiai-secret")
    );
    assert_eq!(
        request
            .headers()
            .get("X-Session-Id")
            .and_then(|value| value.to_str().ok()),
        Some("session-1")
    );
}

#[test]
fn converts_chat_response_to_responses_response() {
    let chat = json!({
        "id": "chatcmpl_1",
        "created": 123,
        "model": "glm-5.1",
        "choices": [{
            "message": {
                "role": "assistant",
                "reasoning_content": "hidden reasoning",
                "content": "I will inspect files.",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "list_files",
                        "arguments": "{\"path\":\".\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 12,
            "prompt_tokens_details": {
                "cached_tokens": 8
            },
            "completion_tokens": 3,
            "completion_tokens_details": {
                "reasoning_tokens": 2
            },
            "total_tokens": 15
        }
    });

    let response = chat_response_to_responses_response(chat).unwrap();

    assert_eq!(response["id"], "chatcmpl_1");
    assert_eq!(response["output"][0]["type"], "message");
    assert_eq!(
        response["output"][0]["content"][0]["text"],
        "I will inspect files."
    );
    assert_eq!(
        response["output"][0]["reasoning_content"],
        "hidden reasoning"
    );
    assert_eq!(response["output"][1]["type"], "function_call");
    assert_eq!(response["output"][1]["call_id"], "call_1");
    assert_eq!(response["output"][1]["name"], "list_files");
    assert_eq!(response["usage"]["input_tokens"], 12);
    assert_eq!(
        response["usage"]["input_tokens_details"]["cached_tokens"],
        8
    );
    assert_eq!(response["usage"]["output_tokens"], 3);
    assert_eq!(
        response["usage"]["output_tokens_details"]["reasoning_tokens"],
        2
    );
    assert_eq!(response["usage"]["total_tokens"], 15);
}

#[test]
fn converts_apply_patch_chat_function_call_to_custom_tool_call() {
    let patch = "*** Begin Patch\n*** Add File: probe.txt\n+ok\n*** End Patch";
    let chat = json!({
        "id": "chatcmpl_1",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_patch",
                    "type": "function",
                    "function": {
                        "name": "apply_patch",
                        "arguments": serde_json::to_string(&json!({ "patch": patch })).unwrap()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });

    let response = chat_response_to_responses_response(chat).unwrap();

    assert_eq!(response["output"][0]["type"], "custom_tool_call");
    assert_eq!(response["output"][0]["call_id"], "call_patch");
    assert_eq!(response["output"][0]["name"], "apply_patch");
    assert_eq!(response["output"][0]["input"], patch);
}

#[test]
fn converts_chat_payload_to_kimi_anthropic_messages() {
    let chat = json!({
        "model": "kimi-for-coding",
        "stream": true,
        "max_tokens": 4096,
        "temperature": 0.2,
        "messages": [
            { "role": "system", "content": "base" },
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "hello" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,aW1n" } }
                ]
            },
            {
                "role": "assistant",
                "content": "checking",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"main.rs\"}" }
                }]
            },
            { "role": "tool", "tool_call_id": "call_1", "content": "file body" }
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read file",
                "parameters": { "type": "object", "properties": { "path": { "type": "string" } } }
            }
        }]
    });

    let anthropic = chat_payload_to_anthropic_payload(chat);

    assert_eq!(anthropic["model"], "kimi-for-coding");
    assert_eq!(anthropic["max_tokens"], 4096);
    assert_eq!(anthropic["system"], "base");
    assert_eq!(anthropic["messages"][0]["role"], "user");
    assert_eq!(anthropic["messages"][0]["content"][0]["text"], "hello");
    assert_eq!(anthropic["messages"][0]["content"][1]["type"], "image");
    assert_eq!(
        anthropic["messages"][0]["content"][1]["source"]["type"],
        "base64"
    );
    assert_eq!(
        anthropic["messages"][0]["content"][1]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(
        anthropic["messages"][0]["content"][1]["source"]["data"],
        "aW1n"
    );
    assert_eq!(anthropic["messages"][1]["role"], "assistant");
    assert_eq!(anthropic["messages"][1]["content"][0]["text"], "checking");
    assert_eq!(anthropic["messages"][1]["content"][1]["type"], "tool_use");
    assert_eq!(anthropic["messages"][1]["content"][1]["name"], "read_file");
    assert_eq!(
        anthropic["messages"][1]["content"][1]["input"]["path"],
        "main.rs"
    );
    assert_eq!(anthropic["messages"][2]["role"], "user");
    assert_eq!(
        anthropic["messages"][2]["content"][0]["type"],
        "tool_result"
    );
    assert_eq!(
        anthropic["messages"][2]["content"][0]["tool_use_id"],
        "call_1"
    );
    assert_eq!(anthropic["tools"][0]["name"], "read_file");
    assert_eq!(
        anthropic["tools"][0]["input_schema"]["properties"]["path"]["type"],
        "string"
    );
    assert!(anthropic.get("stream").is_none());
}

#[test]
fn converts_anthropic_tools_to_chat_completion_tools() {
    let anthropic = json!({
        "model": "deepseek-v4-pro",
        "stream": true,
        "max_tokens": 4096,
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "inspect" }] }
        ],
        "tools": [{
            "name": "Read",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_path": { "type": "string" }
                },
                "required": ["file_path"]
            }
        }],
        "tool_choice": { "type": "auto" }
    });

    let chat = anthropic_payload_to_chat_payload(anthropic);

    assert_eq!(chat["model"], "deepseek-v4-pro");
    assert_eq!(chat["stream"], true);
    assert_eq!(chat["messages"][0]["role"], "user");
    assert_eq!(chat["messages"][0]["content"], "inspect");
    assert_eq!(chat["tools"][0]["type"], "function");
    assert_eq!(chat["tools"][0]["function"]["name"], "Read");
    assert_eq!(
        chat["tools"][0]["function"]["parameters"]["properties"]["file_path"]["type"],
        "string"
    );
    assert_eq!(chat["tool_choice"], "auto");
}

#[test]
fn converts_anthropic_image_blocks_to_chat_image_content() {
    let anthropic = json!({
        "model": "deepseek-v4-pro",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "inspect" },
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/jpeg",
                        "data": "anBn"
                    }
                }
            ]
        }]
    });

    let chat = anthropic_payload_to_chat_payload(anthropic);

    assert_eq!(chat["messages"][0]["role"], "user");
    assert_eq!(chat["messages"][0]["content"][0]["type"], "text");
    assert_eq!(chat["messages"][0]["content"][0]["text"], "inspect");
    assert_eq!(chat["messages"][0]["content"][1]["type"], "image_url");
    assert_eq!(
        chat["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/jpeg;base64,anBn"
    );
}

#[test]
fn converts_anthropic_tool_history_to_chat_completion_messages() {
    let anthropic = json!({
        "model": "deepseek-v4-pro",
        "messages": [
            {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "Read",
                    "input": { "file_path": "README.md" }
                }]
            },
            {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "content": "file body"
                }]
            }
        ]
    });

    let chat = anthropic_payload_to_chat_payload(anthropic);

    assert_eq!(chat["messages"][0]["role"], "assistant");
    assert!(chat["messages"][0]["content"].is_null());
    assert_eq!(chat["messages"][0]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        chat["messages"][0]["tool_calls"][0]["function"]["name"],
        "Read"
    );
    assert_eq!(
        chat["messages"][0]["tool_calls"][0]["function"]["arguments"],
        "{\"file_path\":\"README.md\"}"
    );
    assert_eq!(chat["messages"][1]["role"], "tool");
    assert_eq!(chat["messages"][1]["tool_call_id"], "call_1");
    assert_eq!(chat["messages"][1]["content"], "file body");
}

#[test]
fn converts_kimi_anthropic_response_to_responses_response() {
    let anthropic = json!({
        "id": "msg_1",
        "model": "kimi-for-coding",
        "content": [
            { "type": "text", "text": "I will read it." },
            {
                "type": "tool_use",
                "id": "call_1",
                "name": "read_file",
                "input": { "path": "main.rs" }
            }
        ],
        "usage": { "input_tokens": 12, "output_tokens": 5 }
    });

    let response = anthropic_response_to_responses_response(anthropic);

    assert_eq!(response["id"], "msg_1");
    assert_eq!(response["model"], "kimi-for-coding");
    assert_eq!(response["output"][0]["type"], "message");
    assert_eq!(
        response["output"][0]["content"][0]["text"],
        "I will read it."
    );
    assert_eq!(response["output"][1]["type"], "function_call");
    assert_eq!(response["output"][1]["call_id"], "call_1");
    assert_eq!(response["output"][1]["name"], "read_file");
    assert_eq!(response["output"][1]["arguments"], "{\"path\":\"main.rs\"}");
    assert_eq!(response["usage"]["input_tokens"], 12);
    assert_eq!(response["usage"]["output_tokens"], 5);
    assert_eq!(response["usage"]["total_tokens"], 17);
}

#[test]
fn wraps_non_stream_response_as_responses_sse() {
    let response = json!({
        "id": "resp_1",
        "object": "response",
        "created_at": 123,
        "model": "deepseek-v4-pro",
        "status": "completed",
        "output": [{
            "id": "msg_proxy",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{ "type": "output_text", "text": "done" }]
        }],
        "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 }
    });

    let sse = responses_response_to_sse(&response);
    let text = String::from_utf8(sse).unwrap();

    assert!(text.contains("event: response.output_item.added"));
    assert!(text.contains("event: response.output_text.delta"));
    assert!(text.contains("\"delta\":\"done\""));
    assert!(text.contains("event: response.completed"));
    assert!(text.contains("data: [DONE]"));
}

#[test]
fn converts_chat_stream_to_responses_stream() {
    let body = concat!(
        "data:{\"id\":\"chatcmpl_1\",\"model\":\"glm-5.1\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\".\\\"}\"}}]}}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n",
        "data: [DONE]\n\n"
    );

    let normalized = chat_sse_to_responses_sse(body.as_bytes());
    let text = String::from_utf8(normalized).unwrap();

    assert!(text.contains("event: response.output_text.delta"));
    assert!(text.contains("event: response.function_call_arguments.delta"));
    assert!(text.contains("event: response.function_call_arguments.done"));
    assert!(text.contains("event: response.completed"));
    assert!(text.contains("\"name\":\"list_files\""));
    assert!(text.contains("\"arguments\":\"{\\\"path\\\":\\\".\\\"}\""));
    assert!(text.contains("\"input_tokens\":12"));
    assert!(text.contains("data: [DONE]"));
}

#[test]
fn converts_chat_stream_to_anthropic_stream() {
    let body = concat!(
        "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n",
        "data: [DONE]\n\n"
    );

    let normalized = chat_sse_to_anthropic_sse(body.as_bytes());
    let text = String::from_utf8(normalized).unwrap();

    assert!(text.contains("event: message_start"));
    assert!(text.contains("event: content_block_start"));
    assert!(text.contains("event: content_block_delta"));
    assert!(text.contains("\"type\":\"text_delta\""));
    assert!(text.contains("\"text\":\"hello\""));
    assert!(text.contains("event: content_block_stop"));
    assert!(text.contains("event: message_delta"));
    assert!(text.contains("\"stop_reason\":\"end_turn\""));
    assert!(text.contains("\"input_tokens\":12"));
    assert!(text.contains("event: message_stop"));
}

#[test]
fn preserves_markdown_newlines_in_anthropic_stream() {
    let body = concat!(
        "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"content\":\"核心功能是：\\n\\n1. 第一项\\n\"}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"2. 第二项\"}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
        "data: [DONE]\n\n"
    );

    let normalized = chat_sse_to_anthropic_sse(body.as_bytes());
    let text = String::from_utf8(normalized).unwrap();

    assert!(text.contains("核心功能是：\\n\\n1. 第一项\\n"));
    assert!(text.contains("2. 第二项"));
}

#[test]
fn skips_empty_chat_text_deltas_in_anthropic_stream() {
    let body = concat!(
        "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
        "data: [DONE]\n\n"
    );

    let normalized = chat_sse_to_anthropic_sse(body.as_bytes());
    let text = String::from_utf8(normalized).unwrap();

    assert!(text.contains("event: message_start"));
    assert!(!text.contains("event: content_block_start"));
    assert!(!text.contains("event: content_block_delta"));
    assert!(text.contains("\"stop_reason\":\"end_turn\""));
    assert!(text.contains("event: message_stop"));
}

#[test]
fn converts_chat_tool_stream_to_anthropic_tool_use() {
    let body = concat!(
        "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\".\\\"}\"}}]}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
        "data: [DONE]\n\n"
    );

    let normalized = chat_sse_to_anthropic_sse(body.as_bytes());
    let text = String::from_utf8(normalized).unwrap();

    assert!(text.contains("\"type\":\"tool_use\""));
    assert!(text.contains("\"id\":\"call_1\""));
    assert!(text.contains("\"name\":\"list_files\""));
    assert!(text.contains("\"type\":\"input_json_delta\""));
    assert!(text.contains("\"partial_json\":\"{\\\"path\\\":\""));
    assert!(text.contains("\"partial_json\":\"\\\".\\\"}\""));
    assert!(text.contains("\"stop_reason\":\"tool_use\""));
}

#[test]
fn restores_deepseek_reasoning_for_anthropic_tool_history() {
    let body = concat!(
        "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"reasoning_content\":\"tool thinking\"}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_cached_tool\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"README.md\\\"}\"}}]}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let _ = chat_sse_to_anthropic_sse(body.as_bytes());

    let anthropic = json!({
        "model": "deepseek-v4-pro",
        "messages": [{
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call_cached_tool",
                "name": "Read",
                "input": { "file_path": "README.md" }
            }]
        }]
    });

    let chat = anthropic_payload_to_chat_payload(anthropic);

    assert_eq!(chat["messages"][0]["reasoning_content"], "tool thinking");
}

#[test]
fn converts_chat_stream_incrementally() {
    let mut converter = ChatSseStreamConverter::new();

    let first = converter.push_chunk(
            b"data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"content\":\"hel",
        );
    assert!(first.is_empty());

    let second = converter.push_chunk(b"lo\"}}]}\n\n");
    let second = String::from_utf8(second).unwrap();
    assert!(second.contains("event: response.output_text.delta"));
    assert!(second.contains("\"delta\":\"hello\""));

    let done = String::from_utf8(converter.finish()).unwrap();
    assert!(done.contains("event: response.completed"));
    assert!(done.contains("data: [DONE]"));
}

#[test]
fn preserves_deepseek_stream_reasoning_content() {
    let body = concat!(
        "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"reasoning_content\":\"think \"}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"reasoning_content\":\"more\"}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
        "data: [DONE]\n\n"
    );

    let normalized = chat_sse_to_responses_sse(body.as_bytes());
    let text = String::from_utf8(normalized).unwrap();

    assert!(text.contains("\"model\":\"deepseek-v4-pro\""));
    assert!(text.contains("\"reasoning_content\":\"think more\""));
    assert!(text.contains("\"text\":\"answer\""));
}

#[test]
fn injects_remembered_reasoning_content_into_next_chat_request() {
    let body = concat!(
        "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"reasoning_content\":\"cached thinking\"}}]}\n\n",
        "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"unique cached answer\"}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let _ = chat_sse_to_responses_sse(body.as_bytes());
    let payload = json!({
        "input": [{
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "unique cached answer" }]
        }]
    });

    let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

    assert_eq!(chat["messages"][0]["role"], "assistant");
    assert_eq!(chat["messages"][0]["content"], "unique cached answer");
    assert_eq!(chat["messages"][0]["reasoning_content"], "cached thinking");
}

#[test]
fn leaves_uncached_deepseek_text_history_without_fake_reasoning() {
    let payload = json!({
        "input": [{
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "uncached assistant answer" }]
        }]
    });

    let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

    assert_eq!(chat["messages"][0]["role"], "assistant");
    assert_eq!(chat["messages"][0]["content"], "uncached assistant answer");
    assert!(chat["messages"][0].get("reasoning_content").is_none());
}

#[test]
fn normalizes_deepseek_assistant_messages_before_upstream_request() {
    let payload = json!({
        "model": "deepseek-v4-pro",
        "messages": [
            { "role": "system", "content": "base" },
            { "role": "assistant", "content": "older answer" },
            {
                "role": "assistant",
                "content": "answer with reasoning",
                "reasoning_content": "already present"
            }
        ],
        "stream": true
    });

    let normalized = normalize_chat_payload_for_provider(payload, "deepseek");

    assert!(normalized["messages"][1].get("reasoning_content").is_none());
    assert_eq!(
        normalized["messages"][2]["reasoning_content"],
        "already present"
    );
}

#[test]
fn normalizes_timiai_deepseek_assistant_messages_before_chat_upstream_request() {
    let payload = json!({
        "model": "deepseek-v4-pro-r1",
        "messages": [
            { "role": "system", "content": "base" },
            { "role": "assistant", "content": "older answer" }
        ],
        "stream": true
    });

    let normalized = normalize_chat_payload_for_provider(payload, "timiai");

    assert!(normalized["messages"][1].get("reasoning_content").is_none());
}

#[test]
fn disables_deepseek_thinking_when_tool_reasoning_history_is_missing() {
    let payload = json!({
        "model": "deepseek-v4-pro",
        "messages": [
            { "role": "user", "content": "inspect" },
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_missing_reasoning",
                    "type": "function",
                    "function": { "name": "Read", "arguments": "{\"file_path\":\"README.md\"}" }
                }]
            },
            { "role": "tool", "tool_call_id": "call_missing_reasoning", "content": "file contents" }
        ],
        "reasoning_effort": "max",
        "stream": true
    });

    let normalized = normalize_chat_payload_for_provider(payload, "deepseek");

    assert_eq!(normalized["thinking"]["type"], "disabled");
    assert!(normalized.get("reasoning_effort").is_none());
    assert!(normalized["messages"][1].get("reasoning_content").is_none());
}

#[test]
fn rewrites_xiaomi_anthropic_display_model_to_upstream_slug() {
    let payload = json!({
        "model": "MiMo-V2.5-Pro",
        "messages": [{ "role": "user", "content": "hello" }],
        "stream": true
    });

    let normalized = normalize_native_anthropic_payload(payload, "xiaomi_mimo");

    assert_eq!(normalized["model"], "mimo-v2.5-pro");
}

#[test]
fn rewrites_xiaomi_chat_completion_display_model_to_upstream_slug() {
    let payload = json!({
        "model": "MiMo-V2.5-Pro",
        "messages": [{ "role": "user", "content": "hello" }],
        "stream": true
    });

    let normalized = normalize_chat_payload_for_provider(payload, "xiaomi_mimo");

    assert_eq!(normalized["model"], "mimo-v2.5-pro");
}

#[test]
fn leaves_non_xiaomi_anthropic_model_names_unchanged() {
    let payload = json!({
        "model": "kimi-for-coding",
        "messages": [{ "role": "user", "content": "hello" }]
    });

    let normalized = normalize_native_anthropic_payload(payload, "kimi_code");

    assert_eq!(normalized["model"], "kimi-for-coding");
}

#[test]
fn maps_xiaomi_router_queue_limitation_to_http_429() {
    let body = br#"{
            "error": {
                "code": "429",
                "message": "Cluster rate limit exceeded, request queued but not admitted",
                "type": "router_queue_limitation"
            }
        }"#;

    let status = normalize_upstream_error_status(StatusCode::BAD_REQUEST, body);

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn groups_consecutive_responses_function_calls_before_outputs() {
    let payload = json!({
        "input": [
            { "role": "user", "content": "run tools" },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "list_files",
                "arguments": "{}"
            },
            {
                "type": "function_call",
                "call_id": "call_2",
                "name": "read_file",
                "arguments": "{\"path\":\"README.md\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "files"
            },
            {
                "type": "function_call_output",
                "call_id": "call_2",
                "output": "readme"
            }
        ]
    });

    let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

    assert_eq!(chat["messages"][0]["role"], "user");
    assert_eq!(chat["messages"][1]["role"], "assistant");
    assert_eq!(
        chat["messages"][1]["tool_calls"].as_array().unwrap().len(),
        2
    );
    assert_eq!(chat["messages"][1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(chat["messages"][1]["tool_calls"][1]["id"], "call_2");
    assert_eq!(chat["messages"][2]["role"], "tool");
    assert_eq!(chat["messages"][2]["tool_call_id"], "call_1");
    assert_eq!(chat["messages"][3]["role"], "tool");
    assert_eq!(chat["messages"][3]["tool_call_id"], "call_2");
}

#[test]
fn ignores_unsupported_responses_input_item() {
    let payload = json!({
        "input": [
            {
                "type": "unsupported",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "recovered answer" }]
            },
            {
                "role": "user",
                "content": "hello"
            }
        ]
    });

    let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

    assert_eq!(chat["messages"].as_array().unwrap().len(), 2);
    assert_eq!(chat["messages"][0]["role"], "assistant");
    assert_eq!(chat["messages"][0]["content"], "recovered answer");
    assert!(chat["messages"][0].get("reasoning_content").is_none());
    assert_eq!(chat["messages"][1]["role"], "user");
    assert_eq!(chat["messages"][1]["content"], "hello");
}
