//! Deciding whether a session receives browser tools.
//!
//! The ACP transport for handing a client-hosted tool set to an agent already
//! exists: `SessionConfig.mcp_servers` reaches both `session/new` and
//! `session/load` (see `acp-core::runtime::session_lifecycle`). What this
//! module adds is the rule for *whether* browser tools go in that vector.
//!
//! Four gates, checked in this order so the user gets the most actionable
//! reason first:
//!
//! 1. Is the capability enabled at all?
//! 2. Is this a remote workspace? The browser runs on the local host, so a
//!    remote agent cannot reach it.
//! 3. Does the selected agent accept MCP servers? Only the two managed agents
//!    do; anything else would silently receive tools it cannot call.
//! 4. Does preflight pass? An enabled capability that cannot run must not be
//!    advertised, or the agent will call tools that always fail.
//!
//! Every refusal carries the reason, so the UI can explain the absence instead
//! of the tools just not appearing.

use std::sync::Arc;

use crate::AppPaths;
use crate::browser_preflight::{BrowserPreflight, PreflightState, check_browser};
use workspace_model::PreflightFix;

/// Whether the selected agent understands MCP servers.
///
/// The capability is offered to the two managed agents, which both accept
/// `mcpServers` in `session/new` and `session/load`, plus the harness, which
/// reaches the same servers through its profile patch row instead. Sending the
/// config to an agent that ignores it would look like the feature worked while
/// doing nothing.
pub fn agent_supports_mcp(agent_command: &str) -> bool {
    agent_command == HARNESS_AGENT_COMMAND
        || crate::settings::is_codex_acp_command(agent_command)
        || crate::settings::is_claude_agent_acp_command(agent_command)
}

/// The outcome of resolving browser tools for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserInjection {
    /// Hand these servers to the session.
    Inject { servers: usize },
    /// Do not inject, for a reason worth showing.
    Withheld {
        reason: WithheldReason,
        detail: String,
    },
}

/// Why a session did not receive browser tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithheldReason {
    /// The user has the capability switched off.
    Disabled,
    /// The session runs on a remote workspace.
    RemoteWorkspace,
    /// The selected agent does not accept MCP servers.
    UnsupportedAgent,
    /// Enabled, but the dependency is missing or the configuration is invalid.
    Unavailable,
}

impl WithheldReason {
    /// Stable key for telemetry and the settings UI.
    pub fn as_str(self) -> &'static str {
        match self {
            WithheldReason::Disabled => "disabled",
            WithheldReason::RemoteWorkspace => "remote-workspace",
            WithheldReason::UnsupportedAgent => "unsupported-agent",
            WithheldReason::Unavailable => "unavailable",
        }
    }
}

/// Inputs to the decision, so it can be tested without touching settings.
#[derive(Debug, Clone, Default)]
pub struct BrowserInjectionInput {
    pub enabled: bool,
    pub remote_session: bool,
    pub agent_command: String,
    pub preflight: Option<PreflightState>,
    /// Tools the adapter currently exposes. A session with no exposed tools
    /// gains nothing from the server, so injection is skipped.
    pub exposed_tool_count: usize,
}

/// Resolve browser tools for one session.
pub fn resolve(input: &BrowserInjectionInput) -> BrowserInjection {
    if !input.enabled {
        return withheld(
            WithheldReason::Disabled,
            "Browser tools are turned off in Settings.",
        );
    }

    if input.remote_session {
        return withheld(
            WithheldReason::RemoteWorkspace,
            "Browser tools run on the local host and are not available to remote sessions.",
        );
    }

    if !agent_supports_mcp(&input.agent_command) {
        return withheld(
            WithheldReason::UnsupportedAgent,
            format!(
                "The selected agent does not accept MCP servers, so it cannot call browser tools ({}).",
                input.agent_command
            ),
        );
    }

    if input.preflight.is_none() {
        return withheld(
            WithheldReason::Unavailable,
            "Browser preflight has not run for this session.",
        );
    }

    if let Some(state) = &input.preflight
        && !state.is_ready()
    {
        let (detail, remedy) = match state {
            PreflightState::Missing { detail, remedy, .. }
            | PreflightState::Invalid { detail, remedy, .. } => (detail, remedy),
            PreflightState::Ready => unreachable!("guarded by the is_ready check"),
        };
        return withheld(WithheldReason::Unavailable, format!("{detail} {remedy}"));
    }

    // Deliberately no gate on `exposed_tool_count` here.
    //
    // It looks like the natural check — a session with no exposed tools gains
    // nothing from the server — and it was the last gate before injection. It
    // is also unsatisfiable. The catalog is built from the provider's own
    // `tools/list`, and the provider is only started when a client asks this
    // server for tools, which cannot happen until the server is mounted. So on a
    // correctly provisioned host the count is always zero at this point, and
    // the gate withheld browser tools from every session, on both channels,
    // while every test stayed green because they installed a catalog by hand.
    //
    // Preflight above is the real gate: it answers "can the provider run here"
    // without starting anything. Whether it advertises tools is discovered
    // when the client lists them, and reported then — as a protocol error, not
    // as a silent empty list.
    let _ = input.exposed_tool_count;

    BrowserInjection::Inject { servers: 1 }
}

fn withheld(reason: WithheldReason, detail: impl Into<String>) -> BrowserInjection {
    BrowserInjection::Withheld {
        reason,
        detail: detail.into(),
    }
}

/// Run preflight for the stored settings on the real host.
pub fn preflight_for(app_paths: &AppPaths) -> BrowserPreflight {
    let settings = crate::settings::load_app_settings(app_paths);
    check_browser(
        &settings.browser,
        &crate::browser_preflight::HostEnvironment,
    )
}

/// Build the decision input from live application state.
///
/// `exposed_tool_count` is the adapter's current surface, which is empty until
/// a provider has completed a handshake. Passing the real count rather than a
/// constant is what keeps a session from receiving an empty tool list.
pub fn input_for(
    app_paths: &AppPaths,
    agent_command: &str,
    remote_session: bool,
    exposed_tool_count: usize,
) -> BrowserInjectionInput {
    let settings = crate::settings::load_app_settings(app_paths);
    let preflight = check_browser(
        &settings.browser,
        &crate::browser_preflight::HostEnvironment,
    );

    BrowserInjectionInput {
        enabled: settings.browser.enabled,
        remote_session,
        agent_command: agent_command.to_string(),
        preflight: Some(preflight.state),
        exposed_tool_count,
    }
}

/// Convenience for telemetry: the decision plus its reason key.
pub fn reason_key(injection: &BrowserInjection) -> Option<&'static str> {
    match injection {
        BrowserInjection::Inject { .. } => None,
        BrowserInjection::Withheld { reason, .. } => Some(reason.as_str()),
    }
}

impl From<crate::browser_preflight::BrowserPreflight> for workspace_model::BrowserPreflight {
    fn from(preflight: crate::browser_preflight::BrowserPreflight) -> Self {
        // The state is already the DTO's own type; only the Node path is
        // narrower here, because a preflight check holds a `PathBuf` and the
        // wire carries a string.
        workspace_model::BrowserPreflight {
            state: preflight.state,
            node_executable: preflight
                .node_executable
                .map(|path| path.to_string_lossy().into_owned()),
            provider_version: preflight.provider_version,
        }
    }
}

/// Shared handle type, matching the other managed capabilities.
pub type SharedBrowserAdapter = Arc<crate::browser_mcp::BrowserMcpService>;

/// The harness is not an ACP agent, so it has no agent command to check, but
/// it does reach the same managed servers. This sentinel passes the
/// unsupported-agent gate and nothing else.
pub const HARNESS_AGENT_COMMAND: &str = "dsh-harness";

/// Session id the harness process registers under.
///
/// One registration covers every dsh session, because the harness serves them
/// all from a single process. Each dsh session still acquires its own browser
/// behind that one token; the token gates which *server* a caller can reach,
/// not how many browsers exist.
pub const HARNESS_SESSION_ID: &str = "dsh-harness-process";

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> PreflightState {
        PreflightState::Ready
    }

    fn input() -> BrowserInjectionInput {
        BrowserInjectionInput {
            enabled: true,
            remote_session: false,
            agent_command: "codex-acp".to_string(),
            preflight: Some(ready()),
            exposed_tool_count: 12,
        }
    }

    #[test]
    fn a_ready_session_receives_the_tools() {
        assert_eq!(resolve(&input()), BrowserInjection::Inject { servers: 1 });
        assert_eq!(reason_key(&resolve(&input())), None);
    }

    #[test]
    fn a_disabled_capability_is_withheld() {
        let input = BrowserInjectionInput {
            enabled: false,
            ..input()
        };
        assert_eq!(
            reason_key(&resolve(&input)),
            Some(WithheldReason::Disabled.as_str())
        );
    }

    #[test]
    fn a_remote_session_is_withheld_with_a_local_host_message() {
        let input = BrowserInjectionInput {
            remote_session: true,
            ..input()
        };
        match resolve(&input) {
            BrowserInjection::Withheld { reason, detail } => {
                assert_eq!(reason, WithheldReason::RemoteWorkspace);
                assert!(detail.contains("local host"), "got {detail}");
            }
            other => panic!("expected withheld, got {other:?}"),
        }
    }

    #[test]
    fn an_agent_without_mcp_support_is_withheld() {
        for command in ["codebuddy", "goose", ""] {
            let input = BrowserInjectionInput {
                agent_command: command.to_string(),
                ..input()
            };
            assert_eq!(
                reason_key(&resolve(&input)),
                Some(WithheldReason::UnsupportedAgent.as_str()),
                "agent {command:?} should not receive browser tools",
            );
        }
    }

    #[test]
    fn both_managed_agents_are_supported() {
        assert!(agent_supports_mcp("codex-acp"));
        assert!(agent_supports_mcp("claude-agent-acp"));
    }

    #[test]
    fn a_failed_preflight_withholds_the_tools_and_carries_the_remedy() {
        let input = BrowserInjectionInput {
            preflight: Some(PreflightState::Missing {
                detail: "No Playwright Chromium install was found.".to_string(),
                remedy: "Run `npx playwright install chromium` once, then re-run preflight."
                    .to_string(),
                fix: PreflightFix::Install,
            }),
            ..input()
        };
        match resolve(&input) {
            BrowserInjection::Withheld { reason, detail } => {
                assert_eq!(reason, WithheldReason::Unavailable);
                assert!(detail.contains("npx playwright install"), "got {detail}");
            }
            other => panic!("expected withheld, got {other:?}"),
        }
    }

    #[test]
    fn an_invalid_configuration_is_withheld_before_preflight_runs() {
        let input = BrowserInjectionInput {
            preflight: None,
            ..input()
        };
        assert_eq!(
            reason_key(&resolve(&input)),
            Some(WithheldReason::Unavailable.as_str())
        );
    }

    #[test]
    fn an_empty_catalog_does_not_withhold_injection() {
        // This test used to assert the opposite, and that assertion is why
        // browser tools never reached a session. The catalog is discovered from
        // the provider's own `tools/list`, which cannot run before the server is
        // mounted — so gating on it here can only ever say "no". Preflight is
        // the gate that can actually be answered; whether the provider
        // advertises tools is reported when the client lists them.
        let input = BrowserInjectionInput {
            exposed_tool_count: 0,
            ..input()
        };
        assert_eq!(
            resolve(&input),
            BrowserInjection::Inject { servers: 1 },
            "a provisioned host must not be withheld for having an empty catalog"
        );
    }

    #[test]
    fn a_failed_preflight_still_withholds_regardless_of_the_catalog() {
        // Dropping the catalog gate must not have weakened the gate that
        // matters: an unprovisioned host is still refused.
        let input = BrowserInjectionInput {
            preflight: Some(PreflightState::Missing {
                detail: "no provider".to_string(),
                remedy: "install it".to_string(),
                fix: workspace_model::PreflightFix::Install,
            }),
            exposed_tool_count: 12,
            ..input()
        };
        assert_eq!(
            reason_key(&resolve(&input)),
            Some(WithheldReason::Unavailable.as_str())
        );
    }

    #[test]
    fn disabled_is_checked_before_the_remote_policy() {
        // A disabled capability should say "off", not "not on remote".
        let input = BrowserInjectionInput {
            enabled: false,
            remote_session: true,
            ..input()
        };
        assert_eq!(
            reason_key(&resolve(&input)),
            Some(WithheldReason::Disabled.as_str())
        );
    }

    #[test]
    fn remote_is_checked_before_agent_support() {
        let input = BrowserInjectionInput {
            remote_session: true,
            agent_command: "goose".to_string(),
            ..input()
        };
        assert_eq!(
            reason_key(&resolve(&input)),
            Some(WithheldReason::RemoteWorkspace.as_str())
        );
    }

    #[test]
    fn agent_support_is_checked_before_preflight() {
        // An agent that cannot call MCP should not send the user chasing a
        // browser install.
        let input = BrowserInjectionInput {
            agent_command: "goose".to_string(),
            preflight: Some(PreflightState::Missing {
                detail: "no chromium".to_string(),
                remedy: "install it".to_string(),
                fix: PreflightFix::Install,
            }),
            ..input()
        };
        assert_eq!(
            reason_key(&resolve(&input)),
            Some(WithheldReason::UnsupportedAgent.as_str())
        );
    }

    #[test]
    fn default_settings_withhold_the_tools() {
        // The regression guard: a user who has never opened the browser
        // settings must not get a browser.
        let input = BrowserInjectionInput {
            enabled: false,
            ..Default::default()
        };
        assert_eq!(
            reason_key(&resolve(&input)),
            Some(WithheldReason::Disabled.as_str())
        );
    }

    #[test]
    fn the_harness_sentinel_passes_the_agent_gate() {
        // The dsh channel has no ACP agent command, so it must not be stopped
        // by the unsupported-agent gate — or dsh sessions would never get
        // browser tools at all.
        assert!(agent_supports_mcp(HARNESS_AGENT_COMMAND));
    }

    #[test]
    fn both_channels_reach_the_same_decision_for_the_same_settings() {
        // The parity guard behind task 10.3. The two channels differ only in
        // the agent gate: the harness has no agent command, and it is never
        // remote. Everything else — enablement, preflight, catalog — must
        // produce the same answer, or a user would see different tools
        // depending on which agent they picked.
        let ready = BrowserInjectionInput {
            enabled: true,
            agent_command: "codex-acp".to_string(),
            preflight: Some(ready()),
            exposed_tool_count: 9,
            ..Default::default()
        };
        let harness = BrowserInjectionInput {
            agent_command: HARNESS_AGENT_COMMAND.to_string(),
            ..ready.clone()
        };
        assert_eq!(resolve(&ready), resolve(&harness));

        // And the same holds when the capability is off.
        let disabled = BrowserInjectionInput {
            enabled: false,
            ..ready.clone()
        };
        let harness_disabled = BrowserInjectionInput {
            enabled: false,
            ..harness.clone()
        };
        assert_eq!(
            reason_key(&resolve(&disabled)),
            reason_key(&resolve(&harness_disabled)),
        );
    }

    #[test]
    fn both_channels_agree_when_preflight_fails() {
        let failing = BrowserInjectionInput {
            preflight: Some(PreflightState::Missing {
                detail: "no chromium".to_string(),
                remedy: "install it".to_string(),
                fix: PreflightFix::Install,
            }),
            ..input()
        };
        let harness = BrowserInjectionInput {
            agent_command: HARNESS_AGENT_COMMAND.to_string(),
            ..failing.clone()
        };

        assert_eq!(resolve(&failing), resolve(&harness));
    }

    #[test]
    fn both_channels_agree_when_the_catalog_is_empty() {
        let empty = BrowserInjectionInput {
            exposed_tool_count: 0,
            ..input()
        };
        let harness = BrowserInjectionInput {
            agent_command: HARNESS_AGENT_COMMAND.to_string(),
            ..empty.clone()
        };
        assert_eq!(resolve(&empty), resolve(&harness));
    }
}
