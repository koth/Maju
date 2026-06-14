function isRemoteWorkspaceRoot(value: string) {
  return value.startsWith("ssh://");
}

function normalizeLocalWorkspaceRoot(value: string) {
  const normalized = value.replace(/\\/g, "/");
  if (/^[a-z]:/i.test(normalized)) {
    return normalized.toLowerCase();
  }
  return normalized;
}

export function sameWorkspaceRoot(a: string, b: string) {
  if (isRemoteWorkspaceRoot(a) || isRemoteWorkspaceRoot(b)) {
    return a === b;
  }
  return normalizeLocalWorkspaceRoot(a) === normalizeLocalWorkspaceRoot(b);
}
