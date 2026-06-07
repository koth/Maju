## 1. Settings Model And Migration

- [x] 1.1 Add agent provider profile DTOs to `workspace-model`, including agent family, profile id, display label, proxy kind, selected state, configured state, endpoint/model metadata, and safe help text.
- [x] 1.2 Remove Goose from the built-in `AgentCliId` settings surface while preserving deserialization of legacy Goose settings long enough to migrate them.
- [x] 1.3 Implement settings migration that maps legacy Goose selections to Codex ACP when available and CodeBuddy otherwise.
- [x] 1.4 Implement settings migration from existing Codex provider strings (`default`, `venus`, `deepseek`) to Codex provider profile ids.
- [x] 1.5 Implement settings migration from existing Claude WOA settings to the Claude WOA provider profile while preserving channel, token path, token status, and available models.

## 2. Provider Profile Catalog And Secret Storage

- [x] 2.1 Create a centralized provider profile catalog for Codex profiles: Default, Venus, DeepSeek, Kimi Code, and Xiaomi Token Plan.
- [x] 2.2 Create a centralized provider profile catalog for Claude profiles: WOA, Venus, DeepSeek, Kimi Code, and Xiaomi Token Plan.
- [x] 2.3 Encode proxy kind per profile: `codex_default`, `responses`, `completion_to_responses`, `claude_woa`, `claude_native`, or `completion_to_claude`.
- [x] 2.4 Replace provider-specific credential status booleans with per-profile configured status in the settings snapshot.
- [x] 2.5 Add a generic backend command for saving/replacing provider profile credentials without returning raw secrets.
- [x] 2.6 Keep compatibility wrappers for existing Venus/DeepSeek commands until the frontend is migrated.

## 3. Codex Configuration Generation

- [x] 3.1 Update Codex ACP configuration writing to generate either the Venus channel provider or one BYOK provider backed by configured model-source profiles.
- [x] 3.2 Preserve unmanaged behavior for the Codex Default profile.
- [x] 3.3 Generate Responses-compatible Codex config for profiles with proxy kind `responses`.
- [x] 3.4 Generate completion-to-responses Codex config for profiles with proxy kind `completion_to_responses`.
- [x] 3.5 Add unit tests for generated Codex config/catalog entries for Default, Venus, DeepSeek, Kimi Code, Xiaomi Token Plan, and combined BYOK models.
- [x] 3.6 Route Codex BYOK proxy requests by selected model to the matching configured upstream.

## 4. Claude Launch Configuration

- [x] 4.1 Update Claude Agent ACP launch settings to keep WOA/Venus/BYOK as channels while consuming configured BYOK model-source profiles.
- [x] 4.2 Preserve existing WOA login, refresh, token status, channel, and model behavior under the Claude WOA channel.
- [x] 4.3 Add Claude-native model-source metadata for profiles with proxy kind `claude_native`.
- [x] 4.4 Add completion-to-Claude model-source metadata for profiles with proxy kind `completion_to_claude`.
- [x] 4.5 Add unit tests proving configured BYOK source models are exposed to Claude without switching the WOA channel.

## 5. Tauri Commands And Session Startup

- [x] 5.1 Expose provider profile lists, selected profile ids, and profile credential status through `settings_get_agent_snapshot`.
- [x] 5.2 Add Tauri commands to select a provider profile per agent family and to save profile credentials.
- [x] 5.3 Ensure new Codex sessions launch with the selected Codex provider profile configuration.
- [x] 5.4 Ensure new Claude sessions launch with WOA channel configuration plus configured BYOK model choices.
- [x] 5.5 Ensure changing provider profiles affects only subsequent sessions and does not mutate already running sessions.

## 6. Settings UI

- [x] 6.1 Remove Goose rows, install actions, and selection affordances from the agent settings UI and new workspace agent picker.
- [x] 6.2 Replace Codex provider cards with a Codex channel summary for Default/Venus/BYOK and a BYOK pool explanation.
- [x] 6.3 Keep Claude WOA and Venus as channel controls and move DeepSeek/Kimi/MiMo sources into the BYOK pool.
- [x] 6.4 Show model defaults, configured status, endpoint, and provider help text for the selected BYOK source.
- [x] 6.5 Keep credential inputs write-only, clear them after successful save, and never render raw stored secrets.
- [x] 6.6 Update UI copy so WOA is presented as the Claude channel path, while BYOK is presented as a shared model pool.
- [x] 6.7 Organize CodeBuddy, Codex, and Claude-specific settings into tabs with only the active agent's controls visible.

## 7. Verification

- [x] 7.1 Add Rust tests for settings migration, profile catalog validation, credential redaction, and unsupported profile rejection.
- [x] 7.2 Add frontend tests for Codex and Claude dropdown rendering, selected-profile details, credential save behavior, and Goose removal.
- [x] 7.3 Add session startup tests or integration coverage proving selected Codex and Claude profiles feed into new ACP sessions.
- [x] 7.4 Run `cargo test` from the workspace root.
- [x] 7.5 Run the frontend test suite covering settings and agent selection.
- [x] 7.6 Run `npm --prefix apps/desktop/ui run build`.
