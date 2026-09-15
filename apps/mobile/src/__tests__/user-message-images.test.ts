import { describe, expect, it } from "vitest";
import { hasInlineImage, splitUserMessageBody } from "../features/conversation/user-message-images";

// Mirrors the desktop ConversationTimeline.test.tsx image-splitting cases:
// image markdown is pulled out of the user body so the markdown renderer
// never sees megabyte base64 payloads.

describe("splitUserMessageBody", () => {
  it("splits image-only blocks into images and keeps the text", () => {
    const body = "看看这两张图\n\n![图1](data:image/png;base64,aaaa)\n\n![图2](data:image/jpeg;base64,bbbb)";
    const { text, images } = splitUserMessageBody(body);
    expect(text).toBe("看看这两张图");
    expect(images).toEqual([
      { alt: "图1", src: "data:image/png;base64,aaaa" },
      { alt: "图2", src: "data:image/jpeg;base64,bbbb" },
    ]);
  });

  it("handles an image-only message with no text", () => {
    const { text, images } = splitUserMessageBody("![截图](data:image/png;base64,abcd)");
    expect(text).toBe("");
    expect(images).toEqual([{ alt: "截图", src: "data:image/png;base64,abcd" }]);
  });

  it("tolerates the quoted original-file title appended by the PC", () => {
    const { text, images } = splitUserMessageBody(
      '看这个\n\n![Image: sample.png](data:image/png;base64,dGh1bWI= "file:///Users/x/.kodex/attachments/sample.png")',
    );
    expect(text).toBe("看这个");
    expect(images).toEqual([{ alt: "Image: sample.png", src: "data:image/png;base64,dGh1bWI=" }]);
  });

  it("extracts inline images too, leaving the surrounding text", () => {
    const body = "前文 ![内嵌](data:image/png;base64,aaa) 后文";
    const { text, images } = splitUserMessageBody(body);
    expect(text).not.toContain("data:image");
    expect(text).toContain("前文");
    expect(text).toContain("后文");
    expect(images).toEqual([{ alt: "内嵌", src: "data:image/png;base64,aaa" }]);
  });

  it("extracts images separated by a single newline (no blank line)", () => {
    const body = "只有一行\n![紧贴](data:image/png;base64,abcd)";
    const { text, images } = splitUserMessageBody(body);
    expect(text).toBe("只有一行");
    expect(images).toEqual([{ alt: "紧贴", src: "data:image/png;base64,abcd" }]);
  });

  it("extracts an image with text on the same line after it", () => {
    const body = "![图](data:image/png;base64,abcd) 帮我看看这个";
    const { text, images } = splitUserMessageBody(body);
    expect(text).toBe("帮我看看这个");
    expect(images).toEqual([{ alt: "图", src: "data:image/png;base64,abcd" }]);
  });

  it("does not treat non-image data urls or file links as attachments", () => {
    const body = "![视频](data:video/mp4;base64,aaaa)\n\n![文件](file:///tmp/a.png)";
    const { text, images } = splitUserMessageBody(body);
    expect(text).toBe(body);
    expect(images).toEqual([]);
  });

  it("splits the generated-image block the PC appends to an assistant reply", () => {
    // The PC appends `![生成的图片](data:...)` to the assistant body after a
    // successful generate_image/edit_image. Rendering that through markdown
    // showed megabytes of base64 on the phone.
    const body =
      "图片已经生成好了。\n\n![生成的图片](data:image/png;base64,iVBORw0KGgoAAAA)";
    const { text, images } = splitUserMessageBody(body);
    expect(text).toBe("图片已经生成好了。");
    expect(images).toEqual([
      { alt: "生成的图片", src: "data:image/png;base64,iVBORw0KGgoAAAA" },
    ]);
  });

  it("detects inline images without touching ordinary bodies", () => {
    // The bubble only runs the splitter on bodies that carry an image block,
    // so blank-line collapsing never rewrites prose or fenced code.
    expect(hasInlineImage("普通回复，没有图片")).toBe(false);
    expect(hasInlineImage("```ts\nconst a = 1;\n\n\n\nconst b = 2;\n```")).toBe(false);
    expect(hasInlineImage("![图](data:image/png;base64,aaaa)")).toBe(true);
    // A `g`-flagged regex would leave `lastIndex` behind; alternating calls must
    // keep reporting the same answer.
    expect(hasInlineImage("![图](data:image/png;base64,aaaa)")).toBe(true);
    expect(hasInlineImage("没有图片")).toBe(false);
  });
});
