/**
 * Preserve Windows separators in Markdown image destinations.
 *
 * CommonMark treats `\.` in a destination as an escaped dot and removes the
 * backslash. A perfectly valid local path such as
 * `C:\Users\me\AppData\Local\Temp\.playwright-mcp\shot.png` therefore reaches
 * the renderer as `Temp.playwright-mcp\shot.png`, which is a different file.
 *
 * Parsing happens before React sees the image URL, so repairing `node.url`
 * afterwards cannot recover the lost separator. This plugin reads the raw
 * source slice recorded on the mdast image node and normalizes a local
 * destination to forward slashes before mdast converts it to HAST. The shared
 * image resolver turns those back into the native Windows spelling.
 *
 * Code fences and inline code are untouched because they never produce mdast
 * `image` nodes.
 */

interface MarkdownNode {
  type?: string;
  url?: string;
  position?: {
    start: { offset?: number };
    end: { offset?: number };
  };
  children?: MarkdownNode[];
}

interface MarkdownVFile {
  value?: unknown;
}

function rawImageDestination(source: string): string | null {
  const marker = source.indexOf("](");
  if (marker < 0) return null;
  let rest = source.slice(marker + 2);
  rest = rest.replace(/^\s+/, "");
  if (!rest) return null;

  if (rest.startsWith("<")) {
    let destination = "";
    for (let index = 1; index < rest.length; index += 1) {
      const char = rest[index];
      if (char === "\\" && index + 1 < rest.length) {
        destination += char + rest[index + 1];
        index += 1;
        continue;
      }
      if (char === ">") return destination;
      destination += char;
    }
    return null;
  }

  let destination = "";
  for (let index = 0; index < rest.length; index += 1) {
    const char = rest[index];
    if (char === "\\" && index + 1 < rest.length) {
      // A backslash belongs to the Windows path, not to Markdown. Keep the
      // pair so a following `)` or whitespace cannot truncate the path.
      destination += char + rest[index + 1];
      index += 1;
      continue;
    }
    if (char === ")" || /\s/u.test(char)) break;
    destination += char;
  }
  return destination || null;
}

function normalizeLocalDestination(destination: string): string | null {
  if (/^[A-Za-z]:[\\/]/u.test(destination) || /^file:\/\//iu.test(destination)) {
    return destination.replace(/\\/g, "/");
  }
  return null;
}

function repairImageNode(node: MarkdownNode, source: string): void {
  const start = node.position?.start.offset;
  const end = node.position?.end.offset;
  if (typeof start !== "number" || typeof end !== "number") return;

  const destination = rawImageDestination(source.slice(start, end));
  if (!destination) return;
  const normalized = normalizeLocalDestination(destination);
  if (normalized) node.url = normalized;
}

function walkImages(node: MarkdownNode, source: string): void {
  if (node.type === "image") repairImageNode(node, source);
  for (const child of node.children ?? []) walkImages(child, source);
}

export function remarkLocalImagePaths() {
  return (tree: MarkdownNode, file: MarkdownVFile) => {
    if (typeof file.value !== "string") return;
    walkImages(tree, file.value);
  };
}
