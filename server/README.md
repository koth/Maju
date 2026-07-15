# Kodex Relay Server

公网 WebSocket relay 服务，用于在 Kodex 桌面端（PC）和手机伴侣 app 之间路由端到端加密的控制帧。

- **语言/运行时**：Rust（tokio async）
- **传输**：WebSocket（生产 `wss://`）
- **职责**：设备注册与鉴权、配对绑定、按配对关系路由 `EncryptedEnvelope` 密文帧、账号绑定与订阅状态推送
- **安全红线**：relay 只路由密文，绝不接触 E2E 明文，不生成/持有 E2E 会话密钥

完整需求见 [`docs/relay-service-requirements.md`](../docs/relay-service-requirements.md)，
实现提案见 [`openspec/changes/add-relay-service/`](../openspec/changes/add-relay-service/)。

## 与本仓库的关系

- 依赖共享线协议 [`crates/relay-protocol`](../crates/relay-protocol)，保证 PC / relay / 手机三端 wire 契约一致
- 与 PC 侧 [`crates/relay-client`](../crates/relay-client) 的 mock relay（`spawn_mock_relay`/`spawn_passthrough_relay`）作为协议 conformance 参照
- relay 服务、手机 app 为独立部署，不在 `apps/desktop` 范围内

## 状态

脚手架阶段。完整实现计划见 OpenSpec 提案 `add-relay-service`。依赖版本需与 workspace 根 `Cargo.toml` 对齐后再统一。
