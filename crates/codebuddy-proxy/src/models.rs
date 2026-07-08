use serde_json::{Value, json};
pub fn list_models() -> Vec<Value> {
    vec![
        json!({ "id": "claude-sonnet-5", "object": "model", "created": 0, "owned_by": "codebuddy" }),
        json!({ "id": "claude-opus-4.1", "object": "model", "created": 0, "owned_by": "codebuddy" }),
        json!({ "id": "glm-5.2-ioa", "object": "model", "created": 0, "owned_by": "codebuddy" }),
    ]
}
