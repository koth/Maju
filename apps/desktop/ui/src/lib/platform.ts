export function isMacOS(): boolean {
  if (typeof navigator === "undefined") return false;

  return getNavigatorPlatform().toLowerCase().includes("mac");
}

function getNavigatorPlatform(): string {
  const navigatorWithUserAgentData = navigator as Navigator & {
    userAgentData?: { platform?: string };
  };

  return navigatorWithUserAgentData.userAgentData?.platform ?? navigator.platform ?? navigator.userAgent;
}
