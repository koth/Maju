## Why

The current agent settings page mixes agent installation, provider selection, gateway authentication, and API key entry in one long card list. This makes common choices hard to scan, keeps the niche Goose CLI visible as a first-class option, and does not scale to the provider set needed for Codex and Claude.

This change simplifies the supported agent surface and introduces a shared BYOK model pool. Codex and Claude remain the selectable agent channels. Codex has explicit Default, Venus, and BYOK channels; Claude has explicit WOA, Venus, and BYOK channels. DeepSeek, Kimi Code, Xiaomi Token Plan, and future custom-key services are configured as BYOK model sources.

## What Changes

- Remove Goose from the built-in default agent list, install flow, selection UI, and new-session agent choices.
- Organize agent-specific settings under CodeBuddy, Codex, and Claude tabs; each tab carries its own CLI path, install status, and default-agent action.
- Replace Codex provider cards with a Codex channel summary that offers Default, Venus, and BYOK.
- Keep Claude WOA and Venus as first-class Claude channels, and add configured BYOK models to the Claude BYOK channel instead of switching the whole Claude channel to a single upstream provider.
- Add one BYOK key editor for model sources such as DeepSeek, Kimi Code, and Xiaomi Token Plan.
- Add provider profile metadata for display name, agent family, proxy protocol, base URL, model defaults, key status, and configuration state.
- Support Codex proxy modes:
  - default Codex config with no Kodex-managed provider rewrite
  - completion-to-responses proxy for OpenAI-compatible completion providers
  - native responses-compatible providers when available
- Support Claude proxy modes:
  - WOA gateway
  - completion-to-claude proxy for OpenAI-compatible completion providers
  - native Claude-compatible providers when available
- Include built-in profile presets for Venus, DeepSeek, Kimi Code, and Xiaomi Token Plan for both Codex and Claude where protocol support is defined.
- Keep sensitive provider credentials write-only in the UI: show configured status, allow replacement, never echo secrets.
- Keep existing CodeBuddy, Codex, and Claude managed install behavior. Provider keys affect available models and proxy routing, not which channel is selected.

## Capabilities

### New Capabilities

- `agent-provider-profiles`: Agent settings and session startup support for provider profiles across Codex and Claude, including dropdown selection, proxy protocol metadata, credential status, and generated agent configuration.

### Modified Capabilities

- None.

## Impact

- Affected Rust DTOs and settings logic:
  - `crates/workspace-model/src/lib.rs`
  - `crates/app-core/src/settings.rs`
  - `crates/app-core/src/application.rs`
  - `crates/session-store/src/lib.rs`
  - `apps/desktop/src-tauri/src/commands/settings.rs`
- Affected frontend:
  - `apps/desktop/ui/src/features/settings/SettingsPage.tsx`
  - `apps/desktop/ui/src/features/settings/SettingsPage.css`
  - `apps/desktop/ui/src/types/index.ts`
  - `apps/desktop/ui/src/lib/tauri.ts`
  - new-session agent picker surfaces
- Affected agent packages and generated config:
  - `kodex-claude/` for Claude proxy/WOA startup support if current CLI flags or environment are insufficient.
  - Codex ACP configuration writer for `~/.kodex/config.toml`.
- Persistence impact:
  - Existing selected Goose settings must migrate to a supported default agent.
  - Existing Codex Venus/DeepSeek provider settings must migrate to equivalent Codex provider profiles.
  - Existing Claude WOA channel/token settings must migrate to the Claude WOA provider profile.
- Security impact:
  - Provider keys and tokens remain stored locally and redacted in snapshots, logs, and errors.
