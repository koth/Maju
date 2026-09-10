//! `/compact` dispatch over the typert Remote surface against a fake dsh host.
//!
//! dsh 0.1.5 renamed the `commands/execute` attachment parameter from `images`
//! to `submittedAttachments`; the gateway validates the args object against its
//! generated descriptor and rejects whichever name it does not declare. On dsh
//! 0.1.5.1-rc.1 that surfaced as
//! `gateway/arguments-invalid: … missing "submittedAttachments"; unexpected
//! "images"` and `/compact` failed outright. These tests pin the wire shape the
//! bridge sends, the one-shot fallback, and the fact that the accepted name is
//! remembered for later commands.

mod common;

use common::{MockHarness, default_config};
use dsh_bridge::HttpClient;

fn assert_success(value: Option<dsh_bridge::CommandsExecuteValue>) {
    let value = value.expect("a registered /compact must settle with an execution");
    assert_eq!(value.command_id, "cmd-compact-1");
    match value.result {
        dsh_bridge::CommandsExecuteResult::Success { text, .. } => {
            assert!(text.unwrap_or_default().contains("Compacted"));
        }
        other => panic!("expected success, got {other:?}"),
    }
}

#[tokio::test]
async fn commands_execute_sends_the_current_attachment_field() {
    let mock = MockHarness::start(default_config()).await;
    let client = HttpClient::new(&mock.endpoint()).unwrap();

    let value = client
        .commands_execute("rpc-compact-1".into(), "s-1", "/compact")
        .await
        .unwrap();
    assert_success(value);

    let args = mock.commands_execute_args();
    assert_eq!(args.len(), 1, "the current descriptor must be tried first");
    assert_eq!(args[0]["agentId"], "s-1");
    assert_eq!(args[0]["line"], "/compact");
    assert_eq!(args[0]["submittedAttachments"], serde_json::json!([]));
    assert!(args[0].get("images").is_none());
}

#[tokio::test]
async fn commands_execute_falls_back_to_legacy_images_and_remembers_it() {
    // A harness whose descriptor still declares `images` (dsh ≤ 0.1.4).
    let mut config = default_config();
    config.command_attachment_field = "images".to_string();
    let mock = MockHarness::start(config).await;
    let client = HttpClient::new(&mock.endpoint()).unwrap();

    let value = client
        .commands_execute("rpc-compact-1".into(), "s-1", "/compact")
        .await
        .unwrap();
    assert_success(value);

    let args = mock.commands_execute_args();
    assert_eq!(args.len(), 2, "the rejected shape must be retried once");
    assert_eq!(args[0]["submittedAttachments"], serde_json::json!([]));
    assert!(args[0].get("images").is_none());
    assert_eq!(args[1]["images"], serde_json::json!([]));
    assert!(args[1].get("submittedAttachments").is_none());

    // The accepted name is remembered: the next command skips the rejection.
    let value = client
        .commands_execute("rpc-compact-2".into(), "s-1", "/compact")
        .await
        .unwrap();
    assert_success(value);
    let args = mock.commands_execute_args();
    assert_eq!(args.len(), 3);
    assert_eq!(args[2]["images"], serde_json::json!([]));
    assert!(args[2].get("submittedAttachments").is_none());
}

#[tokio::test]
async fn commands_execute_surfaces_other_gateway_errors_without_retrying() {
    // An args rejection that is NOT about the attachment field must fail as-is:
    // retrying would just add a confusing second failure.
    let mut config = default_config();
    config.commands_execute_error =
        Some("args fields do not match the descriptor: missing \"line\"".to_string());
    let mock = MockHarness::start(config).await;
    let client = HttpClient::new(&mock.endpoint()).unwrap();

    let err = client
        .commands_execute("rpc-compact-1".into(), "s-1", "/compact")
        .await
        .expect_err("the mock rejects the args");
    assert!(
        format!("{err}").contains("arguments-invalid"),
        "the gateway error must reach the caller: {err}"
    );
    assert_eq!(
        mock.commands_execute_args().len(),
        1,
        "an unrelated args rejection must not be retried"
    );
}
