// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import { attachmentKindFor, fileToAttachment } from "./attachments";

function file(name: string, type: string): File {
  return new File(["x"], name, { type });
}

describe("attachmentKindFor (#504)", () => {
  it("classifies images by MIME prefix", () => {
    expect(attachmentKindFor(file("a.png", "image/png"))).toBe("image");
    expect(attachmentKindFor(file("a.jpg", "image/jpeg"))).toBe("image");
  });

  it("classifies documents by MIME type", () => {
    expect(attachmentKindFor(file("r.pdf", "application/pdf"))).toBe(
      "document",
    );
    expect(attachmentKindFor(file("d.csv", "text/csv"))).toBe("document");
    expect(attachmentKindFor(file("c.json", "application/json"))).toBe(
      "document",
    );
  });

  it("falls back to the file extension when MIME is unhelpful", () => {
    expect(
      attachmentKindFor(file("notes.md", "application/octet-stream")),
    ).toBe("document");
    expect(
      attachmentKindFor(file("book.xlsx", "application/octet-stream")),
    ).toBe("document");
    expect(
      attachmentKindFor(file("data.json", "application/octet-stream")),
    ).toBe("document");
  });

  it("returns null for unsupported types", () => {
    expect(attachmentKindFor(file("clip.mp4", "video/mp4"))).toBeNull();
    expect(
      attachmentKindFor(file("a.bin", "application/octet-stream")),
    ).toBeNull();
  });
});

describe("fileToAttachment derives kind (#504)", () => {
  it("tags a PDF as a document, not an image", async () => {
    const att = await fileToAttachment(file("r.pdf", "application/pdf"));
    expect(att.kind).toBe("document");
    expect(att.mediaType).toBe("application/pdf");
  });

  it("tags a PNG as an image", async () => {
    const att = await fileToAttachment(file("a.png", "image/png"));
    expect(att.kind).toBe("image");
  });

  it("rejects an unsupported type instead of mislabeling it", async () => {
    await expect(
      fileToAttachment(file("a.bin", "application/octet-stream")),
    ).rejects.toThrow(/unsupported attachment type/);
  });
});
