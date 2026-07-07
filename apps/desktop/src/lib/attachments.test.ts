// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import {
  attachmentKindFor,
  classifyForStaging,
  describeRejections,
  fileToAttachment,
  ipynbToText,
  isNotebook,
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

describe("source-code attachments (#842)", () => {
  it("accepts .py by MIME and by extension fallback", () => {
    expect(attachmentKindFor(file("s.py", "text/x-python"))).toBe("document");
    expect(attachmentKindFor(file("s.py", "application/x-python-code"))).toBe(
      "document",
    );
    // Browsers often report no MIME for .py — extension carries it.
    expect(attachmentKindFor(file("s.py", ""))).toBe("document");
    expect(attachmentKindFor(file("s.py", "application/octet-stream"))).toBe(
      "document",
    );
  });

  it("accepts .ipynb by extension", () => {
    expect(attachmentKindFor(file("nb.ipynb", ""))).toBe("document");
    expect(attachmentKindFor(file("nb.ipynb", "application/json"))).toBe(
      "document",
    );
  });

  it("classifyForStaging accepts .py and .ipynb when documents are allowed", () => {
    const gate = { visionGated: false, docGated: false };
    expect(classifyForStaging(file("s.py", "text/x-python"), gate)).toBe(
      "document",
    );
    expect(classifyForStaging(file("nb.ipynb", ""), gate)).toBe("document");
  });

  it("still gates .py/.ipynb behind doc support (no silent drop)", () => {
    const gate = { visionGated: false, docGated: true };
    expect(classifyForStaging(file("s.py", "text/x-python"), gate)).toBe(
      "doc-gated",
    );
    expect(classifyForStaging(file("nb.ipynb", ""), gate)).toBe("doc-gated");
  });

  it("isNotebook detects .ipynb only", () => {
    expect(isNotebook(file("nb.ipynb", ""))).toBe(true);
    expect(isNotebook(file("s.py", "text/x-python"))).toBe(false);
    expect(isNotebook(file("d.csv", "text/csv"))).toBe(false);
  });
});

describe("ipynbToText (#842)", () => {
  it("extracts markdown cells as-is and fences code cells", () => {
    const nb = JSON.stringify({
      metadata: { kernelspec: { language: "python" } },
      cells: [
        { cell_type: "markdown", source: ["# Title\n", "intro"] },
        { cell_type: "code", source: ["x = 1\n", "print(x)"] },
      ],
    });
    expect(ipynbToText(nb)).toBe(
      "# Title\nintro\n\n```python\nx = 1\nprint(x)\n```",
    );
  });

  it("drops outputs (execution counts / stdout / base64 images)", () => {
    const nb = JSON.stringify({
      cells: [
        {
          cell_type: "code",
          execution_count: 7,
          source: "plot()",
          outputs: [
            { output_type: "stream", text: ["noisy stdout\n"] },
            { output_type: "display_data", data: { "image/png": "AAAA…" } },
          ],
        },
      ],
    });
    const out = ipynbToText(nb);
    expect(out).toBe("```python\nplot()\n```");
    expect(out).not.toContain("noisy stdout");
    expect(out).not.toContain("AAAA");
  });

  it("skips empty cells and defaults language to python", () => {
    const nb = JSON.stringify({
      cells: [
        { cell_type: "code", source: "" },
        { cell_type: "code", source: "y = 2" },
      ],
    });
    expect(ipynbToText(nb)).toBe("```python\ny = 2\n```");
  });

  it("honors a non-python kernel language", () => {
    const nb = JSON.stringify({
      metadata: { kernelspec: { language: "r" } },
      cells: [{ cell_type: "code", source: "x <- 1" }],
    });
    expect(ipynbToText(nb)).toBe("```r\nx <- 1\n```");
  });

  it("returns the original text on malformed / non-notebook JSON", () => {
    expect(ipynbToText("not json {{{")).toBe("not json {{{");
    expect(ipynbToText(JSON.stringify({ nope: 1 }))).toBe('{"nope":1}');
  });
});

describe("fileToAttachment .ipynb conversion (#842)", () => {
  it("converts a notebook to an inline text/plain document", async () => {
    const nb = JSON.stringify({
      cells: [{ cell_type: "code", source: "print('hi')" }],
    });
    const att = await fileToAttachment(
      new File([nb], "nb.ipynb", { type: "" }),
    );
    expect(att.kind).toBe("document");
    expect(att.mediaType).toBe("text/plain");
    expect(att.source.type).toBe("inline");
    // Decodes back to the converted (fenced) text, not the raw notebook JSON.
    const decoded = new TextDecoder().decode(
      Uint8Array.from(atob(att.source.value), (c) => c.charCodeAt(0)),
    );
    expect(decoded).toBe("```python\nprint('hi')\n```");
    expect(decoded).not.toContain("cell_type");
  });
});
