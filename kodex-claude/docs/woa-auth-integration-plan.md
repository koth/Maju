# WOA Authentication Integration Plan

## 背景与目标

目标是把 `claude-woa` 中已经验证可用的 WOA OAuth Device Code 认证、token 缓存、刷新和网关注入逻辑，内置到当前 TypeScript 版 `@agentclientprotocol/claude-agent-acp` 中，让 ACP 客户端可以直接启动本 agent 并走 `copilot.code.woa.com`，不再依赖外层 `claude-woa.sh` wrapper。

当前仓库里已有一个 OpenSpec 变更 `openspec/changes/add-native-woa-gateway`，但其正文描述的是 Rust crate `claude-code-acp-rs`。本计划以当前代码事实为准：主实现位于 `src/*.ts`，入口是 `src/index.ts`，核心 ACP agent 是 `src/acp-agent.ts`。

非目标：

- 不在运行时调用 `claude-woa/claude-woa.sh` 或 `claude-woa/woa-auth.mjs`。
- 不改变默认非 WOA 行为。
- 不替换现有 ACP session、tool、permission、diff、terminal 等逻辑。
- 不把 WOA 模式做成默认认证方式。

## 现状梳理

### claude-woa 认证与网关注入

`claude-woa/woa-auth.mjs` 实现了完整的 OAuth Device Code 流程：

- 服务端：`https://copilot.code.woa.com`
- client id：`d15f1aada3db4be2be622afed0019a29`
- device code endpoint：`/api/v2/auth/device/code`
- device token endpoint：`/api/v2/auth/device/token`
- refresh endpoint：`/api/v2/auth/oauth_token/refresh`
- token 文件：`~/.claude-woa-token.json`
- token shape：

```json
{
  "accessToken": "...",
  "refreshToken": "...",
  "expiresAt": 1234567890000
}
```

`claude-woa/claude-woa.sh` 在启动官方 `claude` 前做三件事：

1. 根据 `CLAUDE_WOA_CHANNEL` 选择 gateway URL。
2. 执行 token ensure，必要时 refresh。
3. 注入 WOA 所需环境变量：

```text
ANTHROPIC_BASE_URL
ANTHROPIC_AUTH_TOKEN
AUTH_TOKEN
ANTHROPIC_CUSTOM_HEADERS
DISABLE_ERROR_REPORTING
DISABLE_TELEMETRY
DISABLE_AUTOUPDATER
DISABLE_COST_WARNINGS
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC
```

其中 `ANTHROPIC_CUSTOM_HEADERS` 是换行分隔的 `Header: value` 字符串，包含：

```text
x-api-key
x-conversation-id
x-app-version
x-app-name
x-request-platform
x-scene-name
User-Agent
x-request-platform-v2
x-app-name-v2
x-claude-code-internal
x-channel
```

### 当前 ACP 实现的认证入口

`src/index.ts` 目前只处理两类入口：

- 带 `--cli` 时透传到 Claude CLI。
- 否则启动 ACP server，并调用 `runAcp()`。

`src/acp-agent.ts` 已有通用 gateway auth 机制：

- `initialize()` 中只有当客户端声明 `auth._meta.gateway` 时才暴露 `gateway` 和 `gateway-bedrock` auth method。
- `authenticate()` 保存客户端传入的 `GatewayAuthRequest`。
- `createSession()` 构造 Claude Agent SDK `Options` 时，把 `process.env`、用户 `_meta.claudeCode.options.env`、`createEnvForGateway(this.gatewayAuthRequest)` 合并到 `options.env`。
- `createEnvForGateway()` 可注入 `ANTHROPIC_BASE_URL`、`ANTHROPIC_CUSTOM_HEADERS`、`ANTHROPIC_AUTH_TOKEN`，但 token 是占位空格，并且 header 来源完全依赖客户端传入。

这说明 WOA 不需要重写 ACP session 管线，应该作为一个“本地配置驱动的 gateway provider”接入到现有 `createSession()` 的 `options.env` 合并点。

### 当前测试形态

相关测试已覆盖：

- gateway auth method 暴露与隐藏：`src/tests/authorization.test.ts`
- `options.env` 合并顺序：`src/tests/create-session-options.test.ts`
- session 创建、load、resume、fork 等行为：`src/tests/acp-agent.test.ts`、`src/tests/session-load.test.ts`

新增 WOA 实现应延续这些测试风格：用 Vitest mock SDK `query()`，断言传入 `options.env` 和错误行为，而不是在单元测试里访问真实 WOA 网络。

## 目标架构

WOA 集成建议拆成三个层次：

```text
src/index.ts
  ├─ 解析 WOA CLI / env 开关
  ├─ 执行 woa-login / woa-status / woa-refresh 命令
  └─ runAcp({ woa })

src/woa/*
  ├─ config.ts    解析 --woa、channel、tokenPath
  ├─ token.ts     读写 ~/.claude-woa-token.json
  ├─ auth.ts      device code、poll、refresh、ensureToken
  ├─ headers.ts   gateway URL、自定义 headers、env vars
  └─ error.ts     可读错误与 secret redaction

src/acp-agent.ts
  ├─ ClaudeAcpAgent 构造时接收 woaConfig
  ├─ initialize() 可选暴露 WOA terminal auth method
  └─ createSession() 在 session 级别 ensure token 并注入 WOA env
```

关键原则：

- WOA 模式必须显式启用：`--woa` 或 `CLAUDE_ACP_WOA=1`。
- WOA env 应在普通 env 和用户 env 之后注入，确保 WOA gateway URL、token、headers 优先生效。
- 每个 ACP session 都生成独立 `x-conversation-id`。
- token ensure 需要在启动时做一次，也需要在 session 创建时再做一次，以覆盖长时间运行的 editor agent。
- 所有输出、错误、日志都不能泄漏完整 access token、refresh token 或 `ANTHROPIC_CUSTOM_HEADERS`。

## 详细实施步骤

### 1. 新增 WOA 配置模型

新增 `src/woa/config.ts`：

- `WoaChannel = "default" | "offline"`
- `WoaConfig`：

```ts
type WoaConfig = {
  enabled: boolean;
  channel: WoaChannel;
  tokenPath: string;
};
```

- 默认 token path：`path.join(os.homedir(), ".claude-woa-token.json")`
- 默认 channel：`default`
- gateway URL：
  - `default`: `https://copilot.code.woa.com/server/chat/codebuddy-gateway/codebuddy-code`
  - `offline`: `https://copilot.code.woa.com/server/chat/codebuddy-gateway-offline/codebuddy-code`

解析来源：

- CLI：`--woa`、`--woa-channel <default|offline>`、`--woa-token-path <path>`
- env：`CLAUDE_ACP_WOA=1`、`CLAUDE_WOA_CHANNEL`、`CLAUDE_WOA_TOKEN_PATH`

优先级建议：

```text
CLI 显式参数 > 环境变量 > 默认值
```

### 2. 新增 WOA token 存储

新增 `src/woa/token.ts`：

- `WoaToken` 类型兼容 `claude-woa`：

```ts
type WoaToken = {
  accessToken: string;
  refreshToken?: string;
  expiresAt: number;
};
```

- `loadToken(tokenPath): Promise<WoaToken | null>`
- `saveToken(tokenPath, token): Promise<void>`
- `isExpiringSoon(token, thresholdMs = 5 * 60 * 1000): boolean`
- `maskSecret(secret): string`
- token JSON 校验：字段类型不对时返回明确错误，不吞掉 malformed file。
- 保存时：
  - 创建父目录。
  - 写临时文件后 atomic rename。
  - Unix 上设置 `0600`；Windows 上不强行模拟权限。

### 3. 新增 WOA OAuth 实现

新增 `src/woa/auth.ts`：

- 迁移 `woa-auth.mjs` 的逻辑到 TypeScript。
- 使用 Node 18+ 全局 `fetch`，避免新增 HTTP dependency。
- 常量集中定义：

```ts
const WOA_SERVER = "https://copilot.code.woa.com";
const WOA_CLIENT_ID = "d15f1aada3db4be2be622afed0019a29";
```

公开函数：

- `requestDeviceCode(fetchImpl = fetch)`
- `pollForToken(device, fetchImpl = fetch, sleepImpl = sleep)`
- `refreshToken(token, fetchImpl = fetch)`
- `login(config, io?)`
- `refreshAndSave(config)`
- `ensureToken(config)`
- `getTokenStatus(config)`

错误处理：

- `authorization_pending`：继续轮询。
- `slow_down`：轮询间隔最多增加到 15 秒。
- device code 过期：提示重新登录。
- token 缺失或 refresh 失败：提示运行本包的 WOA 登录命令。

### 4. 新增 WOA headers 与 env 构造

新增 `src/woa/headers.ts`：

- `buildWoaCustomHeaders({ token, channel, conversationId })`
- `buildWoaEnv({ token, config, conversationId })`

`buildWoaEnv()` 返回：

```ts
{
  ANTHROPIC_BASE_URL: gatewayUrl,
  ANTHROPIC_AUTH_TOKEN: token.accessToken,
  AUTH_TOKEN: token.accessToken,
  ANTHROPIC_CUSTOM_HEADERS: customHeaders,
  DISABLE_ERROR_REPORTING: "1",
  DISABLE_TELEMETRY: "1",
  DISABLE_AUTOUPDATER: "1",
  DISABLE_COST_WARNINGS: "1",
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1"
}
```

自定义 headers 需要严格复刻 `claude-woa.sh`：

```text
x-api-key: <accessToken>
x-conversation-id: <uuid>
x-app-version: 1.1.7
x-app-name: codebuddy-code
x-request-platform: CodeBuddy-Code
x-scene-name: common_chat
User-Agent: Claude-Code-Internal/1.1.7
x-request-platform-v2: Claude-Code-Internal
x-app-name-v2: claude-code-internal
x-claude-code-internal: true
x-channel: <channel>
```

### 5. 调整入口 CLI

修改 `src/index.ts`：

- 在 `--cli` 分支之前识别本 agent 的 WOA 命令：
  - `--woa-login`
  - `--woa-status`
  - `--woa-refresh`
- 这些命令是进程级命令，执行后退出，不启动 ACP server。
- `--woa` 则启动 ACP server，但带上 `WoaConfig`。
- 建议同时支持一个用户友好的 terminal auth 子命令：

```text
node dist/index.js --woa-login
```

对 `--cli` 分支的建议：

- 第一阶段不让 `--cli` 自动继承 WOA，保持它继续透传官方 Claude CLI。
- 如果后续需要 `claude-agent-acp --cli --woa ...`，再单独加设计，因为它会和当前 `--cli` 透传语义冲突。

### 6. 调整 runAcp 与 ClaudeAcpAgent 构造

修改 `src/acp-agent.ts`：

- 新增 agent options：

```ts
type ClaudeAcpAgentOptions = {
  logger?: Logger;
  woa?: WoaConfig;
};
```

- 构造函数从 `constructor(client, logger?)` 演进到兼容写法：

```ts
constructor(client: AgentSideConnection, loggerOrOptions?: Logger | ClaudeAcpAgentOptions)
```

这样现有测试和外部调用不需要一次性全改。

- `runAcp(options?: { woa?: WoaConfig })` 把配置传给 `new ClaudeAcpAgent(client, { woa })`。

### 7. ACP session 创建时注入 WOA env

修改 `createSession()`：

- 在生成 `sessionId` 后，若 WOA enabled：
  - `const token = await ensureToken(this.woaConfig)`
  - `const woaEnv = buildWoaEnv({ token, config: this.woaConfig, conversationId: sessionId })`
- 合并 env 顺序建议改为：

```ts
env: {
  ...process.env,
  ...userProvidedOptions?.env,
  ...createEnvForGateway(this.gatewayAuthRequest),
  ...woaEnv,
  CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS: "1",
}
```

这样 WOA 显式模式优先于通用 gateway auth。`CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS` 放最后保留 ACP 所需事件。

注意：`sessionId` 对 resume/load 是已有 session id；这也可以作为 `x-conversation-id`。如果希望 fork 一定新 conversation，现有 `forkSession` 已经会生成新 `sessionId`。

### 8. 启动时 token ensure

修改 `src/index.ts` 的非命令、非 `--cli` ACP 启动路径：

- 如果 `woa.enabled`：
  - 启动 ACP 前执行一次 `ensureToken(woa)`。
  - 失败时输出可执行提示，例如：

```text
WOA token is missing or expired. Run:
  claude-agent-acp --woa-login
```

session 创建时仍保留二次 ensure，用于长时间运行的 agent。

### 9. 初始化认证方法展示

`initialize()` 当前会展示官方 Claude login、console login 和通用 gateway auth。WOA 模式下建议：

- 当 `woa.enabled` 且客户端支持 terminal auth 时，新增一个 terminal auth method：

```text
id: "woa-login"
name: "Tencent WOA"
type: "terminal"
args: ["--woa-login"]
```

- 如果支持 `_meta["terminal-auth"]`，同样给出 `command: process.execPath` 和当前入口脚本路径。
- 不需要客户端通过 `authenticate()` 回传 WOA headers；WOA 是本地模式。
- 可保留通用 gateway auth method，除非 `--hide-claude-auth` 或后续产品决策要求隐藏。

### 10. 错误与日志脱敏

新增 `src/woa/error.ts` 或在 `token.ts/auth.ts` 中集中实现：

- `WoaError` 分类：
  - missing token
  - malformed token
  - missing refresh token
  - invalid channel
  - device code request failed
  - token polling failed
  - refresh failed
- 所有错误消息避免包含：
  - 完整 access token
  - 完整 refresh token
  - 完整 `ANTHROPIC_CUSTOM_HEADERS`
- status 输出只显示 masked token、过期时间、剩余分钟数、token path、channel。

## 测试计划

### 单元测试

新增测试文件：

- `src/tests/woa-config.test.ts`
  - 默认配置。
  - env 配置。
  - CLI 参数覆盖 env。
  - invalid channel。

- `src/tests/woa-token.test.ts`
  - 兼容 `accessToken` / `refreshToken` / `expiresAt`。
  - missing file 返回 `null`。
  - malformed file 抛可读错误。
  - expiring soon 判断。
  - save/load roundtrip。
  - secret mask 不泄漏完整值。

- `src/tests/woa-auth.test.ts`
  - mock `fetch` 的 device code 成功。
  - `authorization_pending` 后成功。
  - `slow_down` 调整 interval。
  - refresh response 缺少新 refresh token 时保留旧值。
  - ensureToken：valid、refresh、missing、refresh failed。

- `src/tests/woa-headers.test.ts`
  - default/offline gateway URL。
  - headers 完整性。
  - `x-channel` 正确。
  - `x-conversation-id` 来自 session/conversation id。
  - env vars 完整。

### 集成到现有 ACP 测试

扩展 `src/tests/authorization.test.ts`：

- WOA enabled 时可暴露 `woa-login` terminal auth method。
- WOA enabled 不依赖客户端 `authenticate()`。

扩展 `src/tests/create-session-options.test.ts`：

- WOA env 注入到 SDK `query({ options })`。
- WOA env 覆盖用户提供的 `ANTHROPIC_BASE_URL`、`ANTHROPIC_AUTH_TOKEN`、`ANTHROPIC_CUSTOM_HEADERS`。
- `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS` 仍然存在。
- 每个 `newSession()` 使用不同 conversation id，或至少 fork 使用新 id、resume 使用同 id，按最终决策断言。

新增 `src/tests/index-woa-cli.test.ts` 的可行性需要评估。由于 `src/index.ts` 是 bin 入口且有 top-level await，单元测试可能较重。更稳妥的方案是把 CLI 解析抽到 `src/woa/cli.ts` 后测试纯函数。

### 手工验证

在可访问 WOA 网络的环境中验证：

```bash
npm run build
node dist/index.js --woa-login
node dist/index.js --woa-status
node dist/index.js --woa-refresh
node dist/index.js --woa
```

ACP 客户端验证：

- 新建 session。
- prompt。
- Read/Edit/Write/Bash 工具调用。
- permission request。
- cancel。
- plan mode。
- session list/load/resume/fork。
- default/offline channel。

## 文档更新

更新 `README.md`：

- 新增 WOA Gateway Mode 小节。
- 说明首次登录：

```bash
claude-agent-acp --woa-login
```

- 说明启动：

```bash
claude-agent-acp --woa
```

- 说明状态、刷新、channel、token path：

```bash
claude-agent-acp --woa-status
claude-agent-acp --woa-refresh
claude-agent-acp --woa --woa-channel offline
CLAUDE_ACP_WOA=1 CLAUDE_WOA_CHANNEL=offline claude-agent-acp
```

- 说明 token 文件包含敏感凭据。
- 说明 WOA gateway 可能受内部合规规则约束。

如果继续使用 OpenSpec，也应把 `openspec/changes/add-native-woa-gateway/*` 从 Rust crate 描述修正为 TypeScript package 描述，或者新建一个 TypeScript 专用 change，避免后续执行时误改不存在的 Rust 路径。

## 风险与决策点

1. `--cli` 是否支持 WOA
   - 建议第一阶段不支持，避免改变当前 `--cli` 纯透传语义。
   - 后续可以设计 `--cli --woa`，但需要明确参数剥离和 env 注入顺序。

2. `x-conversation-id` 来源
   - 建议直接用 ACP `sessionId`，保证 session 级稳定且 fork 自然生成新 id。
   - 如果 WOA gateway 要求每次 Claude SDK process 都是新 id，可以改用额外 `randomUUID()`。

3. WOA 与通用 gateway auth 同时存在
   - 建议 WOA 显式模式优先，因为 `--woa` 是进程级用户意图。
   - 通用 gateway auth 仍保留给非 WOA 自定义 gateway。

4. OpenSpec 现有变更与代码语言不一致
   - 这是当前最大的流程风险。
   - 实施前应先修正文档/任务，或者明确这次按 TypeScript 仓库执行，不按 Rust 路径执行。

5. token 输出脱敏
   - `woa-auth.mjs` 当前 login/refresh 成功会打印 token 前 16 位。
   - TypeScript 版应只打印 masked token 和 metadata，避免复制旧脚本的泄漏习惯。

## 推荐实施顺序

1. 补齐 `src/woa/*` 的 config、token、headers 单元能力。
2. 接入 OAuth auth/refresh/ensure，并用 mock fetch 覆盖。
3. 抽出 CLI 解析并接入 `src/index.ts` 的 `--woa-*` 命令。
4. 扩展 `runAcp()` 和 `ClaudeAcpAgent` 构造参数。
5. 在 `createSession()` 注入 WOA env。
6. 补充 README 与测试。
7. 在 WOA 网络环境做手工端到端验证。

这条顺序能先把风险最高的认证和 env 构造隔离验证，再碰 `acp-agent.ts` 这个大文件，减少 session 行为回归面。
