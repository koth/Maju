import { useEffect, useState } from "react";
import { Pressable, Text, View } from "react-native";
import type { TurnCompletionContext, TurnOutcome } from "../../session/turn-completion";
import type { BannerPort } from "./presenter";
import { colors, radius, spacing } from "../theme";

// In-app turn-completion banner (spec: mobile-turn-completion-alerts —
// Context-aware alert presentation, foreground/other-screen row).
//
// A tiny module-level store decouples the non-React alert pipeline from the
// overlay component: `bannerPort.show(...)` is callable from anywhere, and
// `<AlertBannerHost>` (mounted once at the app root) renders the current
// banner. Tapping navigates to the session via the injected `onOpen`.

export interface BannerRequest {
  ctx: TurnCompletionContext;
  outcome: TurnOutcome;
  key: number;
}

const listeners = new Set<(banner: BannerRequest) => void>();

export const bannerPort: BannerPort = {
  show(ctx, outcome) {
    const banner = { ctx, outcome, key: Date.now() };
    for (const l of listeners) l(banner);
  },
};

const AUTO_DISMISS_MS = 5000;

export function AlertBannerHost({
  onOpen,
}: {
  onOpen?: (ctx: TurnCompletionContext) => void;
}) {
  const [banner, setBanner] = useState<BannerRequest | null>(null);

  useEffect(() => {
    const listener = (next: BannerRequest) => setBanner(next);
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  }, []);

  useEffect(() => {
    if (!banner) return;
    const timer = setTimeout(() => setBanner(null), AUTO_DISMISS_MS);
    return () => clearTimeout(timer);
  }, [banner]);

  if (!banner) return null;
  const completed = banner.outcome === "completed";
  return (
    <View
      pointerEvents="box-none"
      style={{
        position: "absolute",
        top: spacing.lg,
        left: spacing.lg,
        right: spacing.lg,
        zIndex: 100,
      }}
    >
      <Pressable
        onPress={() => {
          const ctx = banner.ctx;
          setBanner(null);
          onOpen?.(ctx);
        }}
        style={({ pressed }) => ({
          backgroundColor: colors.surfaceRaised,
          borderRadius: radius.lg,
          padding: spacing.md,
          opacity: pressed ? 0.85 : 1,
          flexDirection: "row",
          alignItems: "center",
          shadowColor: "#000",
          shadowOpacity: 0.5,
          shadowRadius: 20,
          shadowOffset: { width: 0, height: 8 },
          elevation: 10,
        })}
      >
        <View
          style={{
            width: 7,
            height: 7,
            borderRadius: 3.5,
            marginRight: spacing.sm,
            backgroundColor: completed ? colors.success : colors.warn,
          }}
        />
        <View style={{ flex: 1, minWidth: 0 }}>
          <Text style={{ color: colors.text, fontWeight: "600", fontSize: 14 }} numberOfLines={1}>
            {banner.ctx.sessionTitle || "会话"}
          </Text>
          <Text style={{ color: colors.textDim, fontSize: 12, marginTop: 2 }}>
            {completed ? "本轮已完成，点击查看结果" : "本轮已中断，点击查看详情"}
          </Text>
        </View>
      </Pressable>
    </View>
  );
}
