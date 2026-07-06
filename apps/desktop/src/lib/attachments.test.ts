// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import {
  attachmentKindFor,
  classifyForStaging,
  describeRejections,
  fileToAttachment,
} from "./attachments";

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

describe("classifyForStaging (#723)", () => {
  const open = { visionGated: false, docGated: false };

  it("returns the kind when the model accepts it", () => {
    expect(classifyForStaging(file("a.png", "image/png"), open)).toBe("image");
    expect(classifyForStaging(file("r.pdf", "application/pdf"), open)).toBe(
      "document",
    );
  });

  it("flags unsupported types", () => {
    expect(classifyForStaging(file("clip.mp4", "video/mp4"), open)).toBe(
      "unsupported",
    );
  });

  it("flags a recognized kind the model can't accept", () => {
    expect(
      classifyForStaging(file("a.png", "image/png"), {
        visionGated: true,
        docGated: false,
      }),
    ).toBe("vision-gated");
    expect(
      classifyForStaging(file("r.pdf", "application/pdf"), {
        visionGated: false,
        docGated: true,
      }),
    ).toBe("doc-gated");
  });

  it("stages a document the model accepts (kind is not a rejection)", () => {
    expect(
      classifyForStaging(file("r.pdf", "application/pdf"), {
        visionGated: true,
        docGated: false,
      }),
    ).toBe("document");
  });
});

describe("describeRejections (#723)", () => {
  it("is null when nothing was rejected", () => {
    expect(describeRejections([])).toBeNull();
  });

  it("counts and pluralizes unsupported files", () => {
    expect(describeRejections(["unsupported"])).toBe(
      "Skipped 1 file: unsupported type",
    );
    expect(describeRejections(["unsupported", "unsupported"])).toBe(
      "Skipped 2 files: unsupported type",
    );
  });

  it("states the capability reason for gated kinds", () => {
    expect(describeRejections(["vision-gated"])).toBe(
      "This model can't accept images",
    );
    expect(describeRejections(["doc-gated"])).toBe(
      "This model can't accept documents",
    );
  });

  it("combines mixed reasons into one notice", () => {
    expect(describeRejections(["vision-gated", "unsupported"])).toBe(
      "This model can't accept images; skipped 1 file: unsupported type",
    );
  });

  it("collapses both gated kinds so 'this model' doesn't repeat", () => {
    expect(describeRejections(["vision-gated", "doc-gated"])).toBe(
      "This model can't accept images or documents",
    );
    expect(
      describeRejections(["doc-gated", "vision-gated", "unsupported"]),
    ).toBe(
      "This model can't accept images or documents; skipped 1 file: unsupported type",
    );
  });
});
