// Port of the desktop `splitUserMessageBody` (apps/desktop/ui
// ConversationTimeline): messages embed images as self-contained markdown
// blocks (`![alt](data:image/...;base64,...)`). Rendering that body through the
// markdown component shows the raw base64 (the RN markdown parser does not
// reliably handle megabyte data URLs), so image markdown is split out and
// rendered as native thumbnails instead — mirroring the desktop strip.
//
// Two producers use this shape: user prompts with attached images, and the PC's
// `extract_generated_image_markdown`, which appends a `![生成的图片](data:...)`
// block to the assistant message after a successful `generate_image` /
// `edit_image`. Both roles must be split, or the phone renders megabytes of
// base64 as text.

export interface InlineMessageImage {
  alt: string;
  src: string;
}

// Global sweep instead of whole-block matching: the PC composes
// `text\n\n![Image: alt](data:...;base64,... "file://...")`, but coalesced
// patch merges or steering bodies may end up single-newline separated or with
// stray whitespace — a positional parse would silently leave megabyte base64
// in the rendered text. The PC may also append a quoted original-file title
// (`"file://..."`) after the URL — tolerated and ignored (the phone has no
// filesystem access to it).
const IMAGE_MARKDOWN_RE =
  /!\[([^\]]*)\]\((data:image\/(?:apng|avif|bmp|png|jpeg|jpg|gif|webp);base64,[A-Za-z0-9+/=]+)(?:\s+"[^"]*")?\)/gi;

// Non-global copy for the predicate: `test` on a `g`-flagged regex is stateful
// across calls (it advances `lastIndex`), which would make alternate messages
// skip the split.
const IMAGE_MARKDOWN_TEST_RE = new RegExp(IMAGE_MARKDOWN_RE.source, "i");

/**
 * Whether `body` carries at least one inline base64 image block.
 *
 * Callers use this to leave ordinary bodies untouched: splitting trims the body
 * and collapses blank lines, which must not touch prose or fenced code blocks
 * that happen to contain several empty lines.
 */
export function hasInlineImage(body: string): boolean {
  return IMAGE_MARKDOWN_TEST_RE.test(body);
}

export function splitUserMessageBody(body: string): { text: string; images: InlineMessageImage[] } {
  const images: InlineMessageImage[] = [];
  const text = body
    .replace(IMAGE_MARKDOWN_RE, (_match: string, alt: string, src: string) => {
      images.push({ alt: alt.trim(), src });
      return "";
    })
    // Collapse the whitespace left behind where images were pulled out.
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { text, images };
}
