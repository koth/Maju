import type { ExpoConfig, ConfigContext } from "expo/config";

export default ({ config }: ConfigContext): ExpoConfig => ({
  ...config,
  name: "Maju",
  slug: "maju-mobile",
  version: "0.1.4",
  orientation: "portrait",
  icon: "./assets/icon.png",
  scheme: "kodex",
  userInterfaceStyle: "automatic",
  newArchEnabled: true,
  android: {
    package: "com.kodex.mobile",
    versionCode: 5,
    // expo-notifications' config plugin also injects POST_NOTIFICATIONS; the
    // explicit entry keeps the permission visible at the config level.
    permissions: ["POST_NOTIFICATIONS"],
    // SDK 54's prebuild defaults edge-to-edge ON (targetSdk 35), which draws
    // the app under the status bar/cutout and broke the header layout. This
    // app's chrome predates edge-to-edge — keep it off.
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
