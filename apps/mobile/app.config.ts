import type { ExpoConfig, ConfigContext } from "expo/config";

export default ({ config }: ConfigContext): ExpoConfig => ({
  ...config,
  name: "Kodex",
  slug: "kodex-mobile",
  version: "0.1.0",
  orientation: "portrait",
  icon: "./assets/icon.png",
  scheme: "kodex",
  userInterfaceStyle: "automatic",
  newArchEnabled: true,
  android: {
    package: "com.kodex.mobile",
  },
  ios: {
    bundleIdentifier: "com.kodex.mobile",
    supportsTablet: true,
  },
  plugins: ["expo-secure-store"],
  experiments: {
    tsconfigPaths: true,
  },
});
