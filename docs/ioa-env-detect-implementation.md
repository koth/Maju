# IOA 登录前内网检测实现说明

## 1. 目的

本文档说明当前仓库中 **IOA 登录前是否检测公司内网**、**怎么检测**、**前后端分别怎么接入**，方便交给其他 agent 按现有方案复用或重做。

结论先说：

- **有检测**；
- **检测发生在登录页初始化时**；
- **检测逻辑在后端接口 `/auth/env/detect`**；
- **后端通过请求腾讯云一个公开接口，读取返回字段 `isCompanyExportIP` / `isInternal` 来判断是否公司内网或公司出口 IP**；
- **前端根据检测结果决定默认登录方式，并决定是否展示 IOA 登录 tab**。

---

## 2. 当前实现涉及文件

### Backend

- `packages/backend/src/modules/auth/routes.ts`
- `packages/backend/src/modules/auth/ioa-client.ts`
- `packages/backend/src/modules/auth/service.ts`

### Frontend

- `packages/frontend/src/pages/LoginPage.tsx`
- `packages/frontend/src/auth/api.ts`

---

## 3. 整体流程

登录页打开后，前端会并行请求两个接口：

1. `/auth/config`
   - 获取是否开启密码登录等配置
2. `/auth/env/detect`
   - 检测当前访问环境是否是公司出口 IP / 内网环境

然后前端根据 `/auth/env/detect` 返回结果：

- 如果是公司环境：
  - 默认 tab 切到 `ioa`
  - 展示 `IOA 登录`
- 如果不是公司环境：
  - 默认 tab 切到 `password`
  - 不展示 `IOA 登录` tab（当前实现是这样）

---

## 4. 后端检测逻辑

文件：`packages/backend/src/modules/auth/routes.ts`

当前已有接口：

```ts
app.get('/env/detect', async (_req, reply) => {
  try {
    const r = await fetch('https://cloud.tencent.com/auth-api/common/platform', { signal: AbortSignal.timeout(3000) });
    const d: any = await r.json();
    const internal = !!(d?.data?.isCompanyExportIP || d?.data?.isInternal);
    return reply.send(ok({ isCompanyExportIP: internal, loginMethod: internal ? 'ioa' : 'password', timestamp: Date.now() }));
  } catch {
    return reply.send(ok({ isCompanyExportIP: false, loginMethod: 'password', timestamp: Date.now() }));
  }
});
```

### 4.1 它做了什么

后端请求：

```text
https://cloud.tencent.com/auth-api/common/platform
```

然后读取返回 JSON 中的字段：

- `data.isCompanyExportIP`
- `data.isInternal`

只要任意一个为真，就把当前环境视为“公司环境”。

### 4.2 返回结构

成功时返回：

```json
{
  "code": 0,
  "message": "ok",
  "data": {
    "isCompanyExportIP": true,
    "loginMethod": "ioa",
    "timestamp": 1710000000000
  }
}
```

或者：

```json
{
  "code": 0,
  "message": "ok",
  "data": {
    "isCompanyExportIP": false,
    "loginMethod": "password",
    "timestamp": 1710000000000
  }
}
```

### 4.3 超时与兜底

当前实现设置了：

- `AbortSignal.timeout(3000)`

即 3 秒超时。

如果：

- 请求失败
- 超时
- 腾讯云接口异常

则后端统一兜底返回：

```json
{
  "isCompanyExportIP": false,
  "loginMethod": "password",
  "timestamp": 1710000000000
}
```

也就是说当前策略是：

- **检测失败时默认按“非公司环境”处理**；
- **优先保证登录页可用，不阻塞登录页加载**。

---

## 5. 前端接入方式

### 5.1 API 封装

文件：`packages/frontend/src/auth/api.ts`

当前实现：

```ts
export async function detectEnvironment(): Promise<EnvInfo> {
  try {
    return await request(AUTH_API + '/env/detect');
  } catch {
    return { isCompanyExportIP: false, timestamp: Date.now() };
  }
}
```

前端这里也做了一层兜底：

- 如果请求 `/auth/env/detect` 失败；
- 则再次默认成非公司环境。

### 5.2 登录页初始化逻辑

文件：`packages/frontend/src/pages/LoginPage.tsx`

核心逻辑：

```ts
Promise.all([getAuthConfig(), detectEnvironment()]).then(([cfg, env]) => {
  const nextConfig = cfg ?? DEFAULT_AUTH_CONFIG;
  const nextEnv = env ?? DEFAULT_ENV_INFO;
  setAuthConfig(nextConfig);
  setEnvInfo(nextEnv);
  setTab(nextEnv.isCompanyExportIP ? 'ioa' : 'password');
}).catch(() => {
  setAuthConfig(DEFAULT_AUTH_CONFIG);
  setEnvInfo(DEFAULT_ENV_INFO);
});
```

含义是：

- 登录页初始化时，配置和环境检测并行拉取；
- 如果 `isCompanyExportIP = true`，默认切到 `ioa` tab；
- 否则默认切到 `password` tab。

### 5.3 UI 展示逻辑

当前登录页只有在公司环境下才展示 IOA 登录按钮：

```tsx
{envInfo.isCompanyExportIP && (
  <button ...>
    IOA 登录
  </button>
)}
```

也就是说：

- 公司环境：显示 IOA 登录 tab
- 非公司环境：不显示 IOA 登录 tab

这不是 IOA 自身协议要求，而是当前产品层面的限制策略。

---

## 6. IOA 登录本身和“环境检测”是分开的

这个要特别说明，方便其他 agent 不要混淆。

### 6.1 环境检测接口

用于：

- 判断当前访问者是不是处于公司网络环境；
- 决定前端默认展示哪种登录方式。

接口：

- `GET /auth/env/detect`

### 6.2 IOA 登录接口

用于：

- 获取 IOA 登录 URL
- 用回调 code 换取用户信息和业务 token

相关接口：

- `GET /auth/ioa/login-url`
- `POST /auth/login/ioa`

也就是说：

- **内网检测不是在 IOA 回调时做的**；
- **而是在登录前、登录页初始化时做的**；
- **检测结果主要用于 UI/默认策略，不是后端登录校验的唯一关卡**。

---

## 7. 其他 agent 实现时应保留的关键行为

如果让别的 agent 按当前逻辑实现，建议明确要求保留下面几个行为。

### 7.1 后端必须由服务端请求检测接口

不要让前端浏览器直接请求：

```text
https://cloud.tencent.com/auth-api/common/platform
```

原因：

- 统一由后端兜底和超时控制更稳；
- 避免浏览器侧跨域或环境差异；
- 方便后续替换检测源。

### 7.2 检测失败必须降级

失败时不要让登录页挂住。

建议保留当前策略：

- 后端失败 -> 返回 `password`
- 前端失败 -> 也默认非公司环境

### 7.3 不要把环境检测和登录认证耦死

环境检测的作用是：

- 引导 UI
- 选择默认登录方式

不要把它做成：

- “检测不到公司环境就禁止一切 IOA 尝试”

除非产品明确要求必须限制。

当前仓库前端虽然隐藏了 IOA tab，但这是 UI 策略，不代表后端协议层必须强校验。

---

## 8. 推荐给其他 agent 的实现任务描述

如果你要把这个任务转给别的 agent，可以直接给他下面这段要求：

### 任务目标

在登录前增加公司环境检测，用于决定是否默认启用 IOA 登录。

### 后端要求

1. 在 auth 模块新增或保持接口：`GET /auth/env/detect`
2. 后端请求：

```text
https://cloud.tencent.com/auth-api/common/platform
```

3. 从响应中读取：

- `data.isCompanyExportIP`
- `data.isInternal`

4. 只要任意一个为真，则返回：

```json
{ "isCompanyExportIP": true, "loginMethod": "ioa", "timestamp": 123 }
```

否则返回：

```json
{ "isCompanyExportIP": false, "loginMethod": "password", "timestamp": 123 }
```

5. 请求超时设为 3 秒，失败时兜底返回 password。

### 前端要求

1. 登录页初始化时并行请求：
   - `/auth/config`
   - `/auth/env/detect`
2. 如果 `isCompanyExportIP=true`：
   - 默认选中 `ioa`
   - 展示 IOA 登录入口
3. 否则：
   - 默认选中 `password`
   - 可隐藏 IOA 登录入口
4. 前端请求失败时也必须兜底，不影响登录页渲染。

---

## 9. 当前实现的优缺点

### 优点

- 很轻量，接入成本低；
- 对前端友好，登录页一进来就知道默认策略；
- 后端集中控制，容易维护；
- 出错时自动降级，不阻塞用户。

### 缺点 / 风险

- 依赖外部检测接口稳定性；
- “公司出口 IP / 内网”判断逻辑完全依赖第三方返回字段；
- 当前前端是“非公司环境就不显示 IOA”，策略偏硬；
- 返回字段名 `isCompanyExportIP` 实际上兼容了 `isInternal`，语义略混。

---

## 10. 建议的可选优化

如果后续要增强，可以考虑：

### 10.1 返回更明确的字段

当前后端只返回：

- `isCompanyExportIP`
- `loginMethod`

更清晰的版本可以改成：

```json
{
  "isCompanyExportIP": true,
  "isInternal": true,
  "detected": true,
  "recommendedLoginMethod": "ioa",
  "timestamp": 123
}
```

### 10.2 增加 debug 信息

仅开发环境可选返回：

- 检测源 URL
- 请求耗时
- 命中字段是 `isCompanyExportIP` 还是 `isInternal`

### 10.3 UI 上允许手动切换

即便检测为非公司环境，也可以：

- 默认不推荐 IOA
- 但保留“尝试 IOA 登录”入口

这样比完全隐藏更柔和。

---

## 11. 最终结论

当前仓库中，**IOA 登录前是有公司内网 / 公司出口 IP 检测的**。

实现方式是：

1. 后端 `GET /auth/env/detect`
2. 服务端请求 `https://cloud.tencent.com/auth-api/common/platform`
3. 读取 `data.isCompanyExportIP` 或 `data.isInternal`
4. 返回推荐登录方式：`ioa` 或 `password`
5. 前端登录页初始化时读取该结果，并据此决定：
   - 默认 tab
   - 是否显示 IOA 登录入口

如果你要，我下一步还能继续补一版：

- **更适合交给 agent 的“开发任务清单版”**
- 或者 **直接按这个文档去改代码实现一版更清晰的检测接口**

