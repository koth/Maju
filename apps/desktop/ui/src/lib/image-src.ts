import { convertFileSrc, isTauri } from "@tauri-apps/api/core";

/** Sources the webview can fetch without crossing the filesystem boundary. */
const FETCHABLE_SOURCE = /^(?:data:|https?:|asset:|blob:)/i;
const FILE_URL = /^file:\/\//i;
const WINDOWS_DRIVE = /^[A-Za-z]:[\\/]/;

function decodePath(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    // A literal '%' in a file name is not a valid escape. Preserve it rather
    // than dropping the whole image or replacing it with a different path.
    return value;
  }
}

/** Native Windows spelling used by convertFileSrc in the desktop webview. */
function normalizeWindowsPath(value: string): string {
  const normalized = value.replace(/\//g, "\\");
  if (normalized.startsWith("\\\\")) {
    // Preserve the UNC / extended-path prefix, but collapse escaped duplicate
    // separators everywhere after it.
    return `\\\\${normalized.slice(2).replace(/\\{2,}/g, "\\")}`;
  }
  return normalized.replace(/\\{2,}/g, "\\");
}

function normalizeLocalPath(value: string): string {
  if (WINDOWS_DRIVE.test(value) || value.startsWith("\\\\")) {
    return normalizeWindowsPath(value);
  }
  return value.replace(/\\/g, "/");
}

function pathFromFileUrl(source: string): string | null {
  const stripped = source.replace(FILE_URL, "");
  // Drive-letter paths need no URL parser, and this also accepts the common
  // non-standard file://C:/... spelling.
  if (WINDOWS_DRIVE.test(stripped)) return decodePath(stripped);
  if (/^\/[A-Za-z]:[\\/]/.test(stripped)) return decodePath(stripped.slice(1));

  try {
    const url = new URL(source);
    if (url.protocol !== "file:") return null;
    const pathname = decodePath(url.pathname);
    if (/^\/[A-Za-z]:\//.test(pathname)) return pathname.slice(1);
    if (url.hostname && url.hostname !== "localhost") {
      return `\\\\${url.hostname}\\${pathname.replace(/^\/+/, "")}`;
    }
    return pathname;
  } catch {
    return decodePath(stripped);
  }
}

function resolveAgainstWorkspace(path: string, workspaceRoot: string): string | null {
  const root = workspaceRoot.trim().replace(/[\\/]+$/, "");
  if (!root) return null;
  const relative = path.replace(/^[\\/]+/, "");
  if (WINDOWS_DRIVE.test(root) || root.startsWith("\\\\")) {
    return normalizeWindowsPath(`${root}\\${relative}`);
  }
  // POSIX roots must keep POSIX separators; root + '\\' produced a path no
  // file could have on macOS/Linux.
  return `${root}/${relative.replace(/\\/g, "/")}`;
}

/**
 * Resolve any image reference used by chat or a tool result into a URL the
 * desktop webview can actually load.
 *
 * This is deliberately the only local-image conversion used by Markdown,
 * user attachments and tool cards. It handles Windows drives, UNC paths,
 * file:// URLs (including percent-escaped spaces), extension-less attachment
 * objects and workspace-relative paths. Whether a file may be served is then
 * enforced by the native asset protocol's scope, not by a filename extension.
 */
export function webviewImageSrc(source: string, workspaceRoot?: string): string {
  const raw = (source ?? "").trim();
  if (!raw || FETCHABLE_SOURCE.test(raw) || !isTauri()) return raw;

  // mdast's URI normalization percent-encodes backslashes before the component
  // sees them. Decode plain local paths too, not only file:// URLs, or a
  // Windows drive becomes C:%5C... and is mistaken for a relative URI.
  let path = FILE_URL.test(raw) ? pathFromFileUrl(raw) : decodePath(raw);
  if (!path) return raw;

  const absolute = WINDOWS_DRIVE.test(path) || path.startsWith("\\\\") || path.startsWith("/");
  if (!absolute) {
    if (!workspaceRoot) return raw;
    const resolved = resolveAgainstWorkspace(path, workspaceRoot);
    if (!resolved) return raw;
    path = resolved;
  }

  path = normalizeLocalPath(path);
  try {
    return convertFileSrc(path);
  } catch {
    return raw;
  }
}
