import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import type { ConfigContext, ExpoConfig } from "expo/config";
import packageJson from "../../package.json";
import appConfig from "../../app.config";

// 版本号单一来源守卫：改版本号只应改 package.json，app.config.ts 与
// android/app/build.gradle 必须从它派生——历史上两处各写一份字面量，结果
// "改了 package.json 构建出来的版本还是没对上"。
function expectedVersionCode(version: string): number {
  const parts = /^(\d+)\.(\d+)\.(\d+)/.exec(version);
  if (!parts) throw new Error(`version 不是 x.y.z 形式：${version}`);
  return Number(parts[1]) * 10000 + Number(parts[2]) * 100 + Number(parts[3]);
}

describe("版本号单一来源：package.json", () => {
  it("package.json 的 version 是 x.y.z 形式", () => {
    expect(packageJson.version).toMatch(/^\d+\.\d+\.\d+/);
  });

  it("app.config 派生自 package.json（version / versionCode 一致）", () => {
    const config = (appConfig as (ctx: ConfigContext) => ExpoConfig)({
      config: {},
    } as unknown as ConfigContext);
    expect(config.version).toBe(packageJson.version);
    expect(config.android?.versionCode).toBe(
      expectedVersionCode(packageJson.version),
    );
  });

  it("android/app/build.gradle 不再手写版本，动态读取 package.json", () => {
    const gradle = readFileSync(
      new URL("../../android/app/build.gradle", import.meta.url),
      "utf8",
    );
    // 硬编码字面量正是"改了 package.json 构建版本对不上"的根源。
    expect(gradle).not.toMatch(/versionName\s+"/);
    expect(gradle).not.toMatch(/versionCode\s+\d+/);
    // 动态读取 package.json，且换算公式与 app.config.ts 一致。
    expect(gradle).toContain('"../../package.json"');
    expect(gradle).toContain("* 10000");
  });
});
