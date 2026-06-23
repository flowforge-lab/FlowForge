// @vitest-environment jsdom

import {
  render,
  screen,
  fireEvent,
  within,
  cleanup,
} from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { MessageAttachments } from "@/components/message-attachments";
import type { Attachment } from "@/bindings";

// A 1x1 PNG, inline so the preview URL resolves synchronously in jsdom.
const PNG_B64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

const imageAtt: Attachment = {
  kind: "image",
  mediaType: "image/png",
  source: { type: "inline", value: PNG_B64 },
  name: "diagram.png",
  bytes: 2048,
};

const docAtt: Attachment = {
  kind: "document",
  mediaType: "application/pdf",
  source: { type: "path", value: "/tmp/report.pdf" },
  name: "report.pdf",
  bytes: 1536,
};

afterEach(cleanup);

describe("MessageAttachments (#341)", () => {
  it("renders an image attachment as a thumbnail", () => {
    render(<MessageAttachments attachments={[imageAtt]} />);
    const img = screen.getByAltText("diagram.png") as HTMLImageElement;
    expect(img.src).toContain(`data:image/png;base64,${PNG_B64}`);
  });

  it("renders a document attachment as a chip with name, type and size", () => {
    render(<MessageAttachments attachments={[docAtt]} />);
    expect(screen.getByText("report.pdf")).toBeTruthy();
    expect(screen.getByText("PDF · 1.5 KB")).toBeTruthy();
    // No <img> for a document.
    expect(screen.queryByRole("img")).toBeNull();
  });

  it("opens a lightbox preview when an image thumbnail is clicked, then closes", () => {
    render(<MessageAttachments attachments={[imageAtt]} />);
    // Closed initially: no preview image.
    expect(screen.queryByAltText("Attachment preview")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Open diagram.png" }));

    const dialog = screen.getByRole("dialog");
    const preview = within(dialog).getByAltText(
      "Attachment preview",
    ) as HTMLImageElement;
    expect(preview.src).toContain(PNG_B64);

    fireEvent.click(screen.getByRole("button", { name: "Close preview" }));
    expect(screen.queryByAltText("Attachment preview")).toBeNull();
  });
});
