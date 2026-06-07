## Context

Kodex currently exposes ACP agent selection through `AgentCliId` and a detected agent list. The built-in agents include CodeBuddy, Goose, Codex ACP, and Claude Agent ACP. Goose is presented next to the primary agents even though it is not part of the desired mainstream workflow.

Provider configuration is split across agent-specific settings. Codex ACP has a hard-coded provider concept with `default`, `venus`, and `deepseek`; the settings UI renders provider cards and writes `~/.kodex/config.toml`. Claude configuration is currently centered on WOA channel/token management, while proxy-backed non-WOA providers are not modeled as first-class choices.

The requested end state is a smaller default agent list and an explicit BYOK model-source model. Codex and Claude remain channel choices. Codex has three channel modes: Default, Venus, and BYOK. Claude has three channel modes: WOA, Venus, and BYOK. Provider brands such as DeepSeek, Kimi Code, and Xiaomi Token Plan are configured once as custom-key BYOK model sources. Profiles still describe endpoint and proxy behavior, but the UI no longer presents those BYOK sources as per-channel provider switches.

## Goals / Non-Goals

**Goals:**

- Remove Goose from default supported agent choices and selection/install UI.
- Introduce a shared provider profile DTO that can represent Codex and Claude provider choices.
- Allow Codex to use the system Default channel, the Venus channel, or the BYOK channel in generated Codex config, with the local proxy routing BYOK requests by selected model to DeepSeek, Kimi Code, Xiaomi Token Plan, or future configured sources.
- Allow Claude to use WOA, Venus, or BYOK as channels while configured BYOK sources append their models to the Claude BYOK channel.
- Make proxy protocol explicit per profile and per agent family.
- Treat Kimi Code as model `kimi-for-coding`; the local Codex proxy converts Codex requests to Kimi's Anthropic Messages endpoint under `https://api.kimi.com/coding/v1`, matching the behavior observed in kabot.
- Preserve safe credential handling: keys/tokens are never echoed in snapshots or logs.
- Migrate existing Codex provider settings and Claude WOA settings into channel settings and the BYOK pool.

**Non-Goals:**

- Do not remove CodeBuddy, Codex ACP, or Claude Agent ACP.
- Do not implement every provider's remote proxy service in this change; the app configures known endpoints and formats.
- Do not make provider profile selection per individual chat session in the first implementation unless the current settings model already supports it cheaply.
- Do not expose raw generated TOML/env blobs as the primary UI.
- Do not implement provider marketplace download/update behavior.

## Decisions

### Decision: Replace hard-coded provider fields with BYOK model-source profiles

Add a provider profile model with stable ids, labels, agent family, proxy kind, endpoint metadata, model defaults, credential fields, and configured status. The profiles are used as model-source metadata for the BYOK pool instead of as the primary Codex/Claude channel selector.

Rationale: The existing `CodexAcpSettingsStatus` has provider-specific booleans such as `venus_key_configured` and `deepseek_key_configured`. This does not scale to Kimi Code, Xiaomi Token Plan, or Claude provider variants. A profile array lets the UI render one BYOK source editor without adding new UI branches for every provider.

Alternative considered: Continue adding booleans and cards per provider. Rejected because the settings page is already visually noisy and each new provider would duplicate backend DTOs, Tauri commands, and React handlers.

### Decision: Keep model-source metadata scoped by agent family

Profiles belong to an agent family: `codex` or `claude`. The same provider brand can have two profiles when the proxy behavior differs, for example Venus for Codex and Venus for Claude.

Rationale: Codex and Claude need different config generation. Codex may use Responses or completion-to-responses. Claude may use WOA, Claude-native, or completion-to-Claude. Sharing a single profile object across both would hide important protocol differences.

Alternative considered: A global provider dropdown independent of agent. Rejected because the channel remains Codex or Claude; provider brands only contribute models and routing metadata.

### Decision: Codex keeps Venus separate from BYOK

Codex generated config uses `model_provider = "venus"` for the Venus channel. For BYOK, it uses `model_provider = "byok"` and points to the local proxy. The BYOK model catalog is built from configured BYOK sources only. The proxy chooses the upstream provider from the requested model and uses the matching stored key.

Rationale: Venus is a first-class Codex channel, while DeepSeek, Kimi Code, and Xiaomi Token Plan are user-key sources under BYOK. The user should not have to switch the entire Codex channel to each BYOK upstream.

Saving or replacing a Codex BYOK source credential updates the stored source entry and refreshes the BYOK catalog when BYOK is already active; it does not change the selected Codex channel or selected BYOK model source. Explicit channel selection remains a separate action.

### Decision: Claude keeps WOA and Venus separate from BYOK

Saving a Claude BYOK source stores the source secret and does not switch Claude away from WOA or Venus. Claude launch config keeps WOA and Venus as first-class channel profiles, while BYOK source models are gathered separately for the BYOK channel.

Rationale: WOA is the Claude gateway/login flow, Venus is an internal Claude model channel, and DeepSeek/Kimi/MiMo are user-key BYOK sources. This keeps channel selection and model-source configuration separate.

### Decision: Model proxy protocol explicitly

Introduce explicit proxy kinds:

- `codex_default`: do not write managed provider config.
- `responses`: provider exposes a Responses-compatible API for Codex.
- `completion_to_responses`: provider exposes completion/chat-completion API and a proxy adapts it to Responses for Codex.
- `claude_woa`: Claude Agent ACP launches through WOA authentication/gateway.
- `claude_native`: provider exposes a Claude-compatible API.
- `completion_to_claude`: provider exposes completion/chat-completion API and a proxy adapts it to Claude format.

Rationale: The user's desired behavior is not just provider selection; it is provider selection plus protocol adaptation. Encoding this in profile metadata avoids burying it in stringly typed UI labels.

Alternative considered: Store only `base_url` and infer protocol from provider id. Rejected because providers can offer multiple proxy surfaces and future profiles may share endpoints with different protocol adapters.

### Decision: Xiaomi Token Plan uses current public endpoints and model names

Use `https://token-plan-cn.xiaomimimo.com/v1` as the OpenAI-compatible base URL for Codex BYOK display/config metadata, with the local Codex proxy sending upstream chat completion requests to `/v1/chat/completions`. Use `https://token-plan-cn.xiaomimimo.com/anthropic` as the Claude Anthropic-compatible base URL. The built-in model choices are displayed as `MiMo-V2.5-Pro` and `MiMo-V2.5`, with `MiMo-V2.5-Pro` as the default, while Codex model slugs sent to the API use the lowercase Xiaomi ids `mimo-v2.5-pro` and `mimo-v2.5`.

Rationale: Xiaomi Token Plan exposes separate OpenAI-compatible and Anthropic-compatible surfaces. Claude should use the Anthropic-compatible surface natively rather than the completion-to-Claude adapter.

### Decision: Use one credential command shape for profile secrets

Replace provider-specific save commands with a generic command such as `settings_save_agent_provider_secret(agent_family, profile_id, secret)`, while keeping compatibility wrappers where needed during migration.

Rationale: The UI needs a single credential editor for the selected profile. Backend code can keep storage paths provider-specific internally, but the command surface should not require new functions for every profile.

Alternative considered: Add `settings_save_kimi_key`, `settings_save_mimo_key`, and parallel Claude variants. Rejected because it repeats the current DeepSeek/Venus scaling problem.

### Decision: Migrate Goose selections to Codex ACP when available, otherwise CodeBuddy

If persisted settings contain `selected_agent = goose`, migrate to Codex ACP when its managed binary is available or installable. If Codex ACP is unavailable, fall back to CodeBuddy.

Rationale: Removing Goose must not leave settings in an invalid enum state. Codex is the closest general-purpose coding agent choice in the redesigned list.

Alternative considered: Keep Goose hidden but valid. Rejected because hidden persisted choices create confusing behavior when users cannot see or change the selected agent.

### Decision: Preserve generated config ownership boundaries

Codex profile selection writes only Codex-owned config such as `~/.kodex/config.toml`. Claude profile selection writes Kodex app settings and launches Claude Agent ACP with the selected profile's env/flags; changes to `kodex-claude` are limited to missing adapter support required by those env/flags.

Rationale: The app should not mix Codex and Claude config formats. Each agent family keeps a clear generation path.

Alternative considered: Write all provider configuration into one Kodex settings file and inject everything at runtime. Rejected for Codex because existing Codex ACP behavior expects config TOML and users may also inspect or edit that file.

## Risks / Trade-offs

- Provider endpoints and protocol assumptions may drift from real services. → Keep built-in profiles data-driven and centralize defaults so updates are small and testable.
- Migrating away from Goose may surprise existing Goose users. → Provide a one-time migration path and clear release note; keep custom `ACP_AGENT_COMMAND` override available for manual Goose use if already supported.
- A generic profile model can become too abstract. → Limit v1 fields to the known needs: label, agent family, proxy kind, base URL, model defaults, credential status, and help text.
- Credential storage may become fragmented across old and new paths. → Define migration tests for old Codex Venus/DeepSeek and Claude WOA settings, and keep redaction behavior at the DTO boundary.
- Completion-to-* proxy behavior may be confused with provider-native behavior. → Display proxy kind in profile detail and require tests for generated Codex/Claude config per proxy kind.

## Migration Plan

1. Add new profile DTOs and defaults while continuing to read existing settings.
2. Map existing Codex provider values:
   - `default` → Codex Default profile
   - `venus` → Codex Venus profile
   - `deepseek` → Codex DeepSeek profile
3. Map existing Claude WOA settings to the Claude WOA profile and preserve channel/token path/model list.
4. On settings load, migrate `selected_agent = goose` to Codex ACP if available, otherwise CodeBuddy.
5. Replace the settings UI with Codex/Claude channel summaries plus one BYOK source editor after backend snapshots expose profile status.
6. Remove old Goose install/detect UI and old provider-card rendering once tests cover the new flow.
7. Rollback by preserving old fields during the first migration step so older app versions can still read settings where possible.

## Open Questions

- Should provider profiles support user-created custom profiles in v1, or only built-in profiles plus editable credentials?
- Should the selected provider profile be global per agent family, or later overrideable per workspace/session?
- Should Claude WOA remain visually named "WOA" or be presented as "Tencent WOA" in the provider dropdown?
