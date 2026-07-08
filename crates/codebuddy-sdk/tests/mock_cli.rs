use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use serde_json::{Value, json};
/// Spawn a mock CLI that speaks stream-json over stdio.
///
/// The mock:
/// - On reading a `control_request{subtype:"initialize"}`, replies with a
///   `control_response` carrying `currentModelId` and emits a `system/init`
///   message with a session_id.
/// - On reading a `type:"user"` message, emits an `assistant` message with
///   a text block, then a `result` message.
/// - On reading a `control_request{subtype:"interrupt"}`, replies with a
///   success `control_response`.
/// - On reading a `control_request{subtype:"mcp_message"}`, replies with a
///   `control_response` carrying the `mcp_response` (id-less ack for
///   notifications, the handler result for requests).
pub struct MockCli {
    pub stdin: std::process::ChildStdin,
    pub stdout: BufReader<std::process::ChildStdout>,
    pub child: std::process::Child,
}
impl MockCli {
    pub fn spawn() -> std::io::Result<Self> {
        let script = mock_script();
        let mut child = Command::new("python")
            .arg("-c")
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Ok(Self { stdin, stdout, child })
    }
    pub fn write(&mut self, line: &str) {
        let _ = self.stdin.write_all(line.as_bytes());
        let _ = self.stdin.write_all(b"\n");
        let _ = self.stdin.flush();
    }
    pub fn read_line(&mut self) -> Option<String> {
        let mut buf = String::new();
        match self.stdout.read_line(&mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    self.read_line()
                } else {
                    Some(trimmed.to_string())
                }
            }
        }
    }
}
fn mock_script() -> String {
    r#"
import sys, json, uuid
session_id = str(uuid.uuid4())
model = "mock-model"
initialized = False
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    t = msg.get("type","")
    if t == "control_request":
        req = msg.get("request",{})
        subtype = req.get("subtype","")
        rid = msg.get("request_id","")
        if subtype == "initialize":
            resp = {"type":"control_response","response":{"subtype":"success","request_id":rid,"response":{"currentModelId":model}}}
            print(json.dumps(resp), flush=True)
            init = {"type":"system","subtype":"init","session_id":session_id,"model":model,"tools":[],"permissionMode":"bypassPermissions"}
            print(json.dumps(init), flush=True)
        elif subtype == "mcp_message":
            inner = req.get("message",{})
            has_id = "id" in inner and inner["id"] is not None
            if has_id:
                method = inner.get("method","")
                if method == "tools/call":
                    result = {"jsonrpc":"2.0","id":inner["id"],"result":{"content":[{"type":"text","text":"mock-result"}]}}
                elif method == "tools/list":
                    result = {"jsonrpc":"2.0","id":inner["id"],"result":{"tools":[]}}
                else:
                    result = {"jsonrpc":"2.0","id":inner["id"],"result":{}}
            else:
                result = {"jsonrpc":"2.0","result":{}}
            resp = {"type":"control_response","response":{"subtype":"success","request_id":rid,"response":{"mcp_response":result}}}
            print(json.dumps(resp), flush=True)
        else:
            resp = {"type":"control_response","response":{"subtype":"success","request_id":rid,"response":{}}}
            print(json.dumps(resp), flush=True)
    elif t == "user":
        assistant = {"type":"assistant","session_id":session_id,"message":{"id":"msg-1","type":"message","role":"assistant","model":model,"content":[{"type":"text","text":"Hello from mock"}],"stop_reason":"end_turn","stop_sequence":None,"usage":{"input_tokens":10,"output_tokens":5}},"parent_tool_use_id":None}
        print(json.dumps(assistant), flush=True)
        result = {"type":"result","subtype":"success","session_id":session_id,"is_error":False,"usage":{"input_tokens":10,"output_tokens":5}}
        print(json.dumps(result), flush=True)
"#.to_string()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mock_cli_initialize_and_one_turn() {
        let mut cli = MockCli::spawn().expect("spawn mock");
        // send initialize
        cli.write(r#"{"type":"control_request","request_id":"ctrl_0","request":{"subtype":"initialize","hasPrompt":true}}"#);
        // expect control_response
        let line1 = cli.read_line().expect("control_response");
        let v: Value = serde_json::from_str(&line1).unwrap();
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["subtype"], "success");
        // expect system/init
        let line2 = cli.read_line().expect("system/init");
        let v: Value = serde_json::from_str(&line2).unwrap();
        assert_eq!(v["type"], "system");
        assert_eq!(v["subtype"], "init");
        // send user message
        cli.write(r#"{"type":"user","session_id":"test","message":{"role":"user","content":"hi"},"parent_tool_use_id":null}"#);
        // expect assistant
        let line3 = cli.read_line().expect("assistant");
        let v: Value = serde_json::from_str(&line3).unwrap();
        assert_eq!(v["type"], "assistant");
        // expect result
        let line4 = cli.read_line().expect("result");
        let v: Value = serde_json::from_str(&line4).unwrap();
        assert_eq!(v["type"], "result");
        let _ = cli.child.kill();
    }
}
