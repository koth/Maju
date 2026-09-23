import type { ExpoConfig, ConfigContext } from "expo/config";
// 版本号单一来源：package.json 的 `version`。改版本号只改 package.json 一处
// ——本配置与 android/app/build.gradle 都从它派生，换算规则两处保持一致：
//   versionCode = major*10000 + minor*100 + patch   （0.1.4 → 104）
// 注意：`expo prebuild --clean` 会重新生成 android/ 工程并把版本重新冻结成
// 字面量——重新生成后请把 build.gradle 里的动态读取块保留/恢复回来（见该文
// 件注释），否则又会回到"改了 package.json 构建版本对不上"的老问题。
import packageJson from "./package.json";

const version = packageJson.version;
const versionParts = /^(\d+)\.(\d+)\.(\d+)/.exec(version);
if (!versionParts) {
  throw new Error(`package.json 的 version 必须是 x.y.z 形式，当前：${version}`);
}
const versionCode =
  Number(versionParts[1]) * 10000 +
  Number(versionParts[2]) * 100 +
  Number(versionParts[3]);

export default ({ config }: ConfigContext): ExpoConfig => ({
  ...config,
  name: "Maju",
  slug: "maju-mobile",
  version,
  orientation: "portrait",
  icon: "./assets/icon.png",
  scheme: "kodex",
  userInterfaceStyle: "automatic",
  newArchEnabled: true,
  android: {
    package: "com.kodex.mobile",
    versionCode,
    // expo-notifications' config plugin also injects POST_NOTIFICATIONS; the
    // explicit entry keeps the permission visible at the config level.
    permissions: ["POST_NOTIFICATIONS"],
    // SDK 54's prebuild defaults edge-to-edge ON (targetSdk 35), which draws
    // the app under the status bar/cutout and broke the header layout. This
    // app's chrome predates edge-to-edge — keep it off. SDK 54 的类型把该字段
    // 收窄为字面量 `true`（false 被标弃用），但运行时 `false` 依旧生效。
    // @ts-expect-error - expo types only allow `true`; runtime honors `false`.
    edgeToEdgeEnabled: false,
  },
  // Dark app chrome: the prebuild default white status bar clashes with the
  // dark header — match the surface color with light content.
  androidStatusBar: {
    backgroundColor: "#11131f",
    barStyle: "light-content",
  },
  ios: {
    bundleIdentifier: "com.kodex.mobile",
    buildNumber: String(versionCode),
    supportsTablet: true,
  },
  plugins: [
    "expo-secure-store",
    "expo-image-picker",
    "expo-audio",
    [
      "expo-notifications",
      {
        icon: "./assets/icon.png",
        color: "#4f8cff",
        // Android notification-channel sounds live in res/raw — resource names
        // must be lowercase [a-z0-9_.], hence underscores, not hyphens.
        sounds: ["./assets/turn_complete.wav", "./assets/turn_interrupted.wav"],
      },
    ],
  ],
  experiments: {
    tsconfigPaths: true,
  },
});
