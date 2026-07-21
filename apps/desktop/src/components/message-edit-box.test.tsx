// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { MessageEditBox } from "@/components/message-edit-box";

const SEED = "what is a phenotype?";

afterEach(cleanup);

describe("MessageEditBox (#929 A)", () => {
  it("seeds the textarea with the message text and offers visible Save/Cancel", () => {
    render(
      <MessageEditBox
        initialText={SEED}
        onSave={() => {}}
        onCancel={() => {}}
      />,
    );
    const box = screen.getByLabelText("Edit message") as HTMLTextAreaElement;
    expect(box.value).toBe(SEED);
    // Both actions are real buttons — keyboard accelerators are never the only way.
    expect(screen.getByRole("button", { name: "Save" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeTruthy();
  });

  it("states that saving discards the responses below — no branch wording", () => {
    render(
      <MessageEditBox
        initialText={SEED}
        onSave={() => {}}
        onCancel={() => {}}
      />,
    );
    const note = screen.getByText(/replaces this message and re-runs/i);
    expect(note.textContent).toMatch(/discarded/i);
    // FlowForge truncates and re-runs; there is no branch/variant model.
    expect(document.body.textContent).not.toMatch(/branch/i);
  });

  it("Save hands back the edited text", () => {
    const onSave = vi.fn();
    render(
      <MessageEditBox initialText={SEED} onSave={onSave} onCancel={() => {}} />,
    );
    fireEvent.change(screen.getByLabelText("Edit message"), {
      target: { value: "what is a genotype?" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(onSave).toHaveBeenCalledWith("what is a genotype?");
  });

  it("Cancel is a no-op: it never saves", () => {
    const onSave = vi.fn();
    const onCancel = vi.fn();
    render(
      <MessageEditBox initialText={SEED} onSave={onSave} onCancel={onCancel} />,
    );
    fireEvent.change(screen.getByLabelText("Edit message"), {
      target: { value: "abandoned draft" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalled();
    expect(onSave).not.toHaveBeenCalled();
  });

  it("disables Save on blank text but allows an unchanged re-run", () => {
    const onSave = vi.fn();
    render(
      <MessageEditBox initialText={SEED} onSave={onSave} onCancel={() => {}} />,
    );
    const save = screen.getByRole("button", {
      name: "Save",
    }) as HTMLButtonElement;
    // Unchanged text is a legitimate submission (re-run the same prompt).
    expect(save.disabled).toBe(false);

    fireEvent.change(screen.getByLabelText("Edit message"), {
      target: { value: "   " },
    });
    expect(
      (screen.getByRole("button", { name: "Save" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(onSave).not.toHaveBeenCalled();
  });

  it("Enter saves and Escape cancels (accelerators)", () => {
    const onSave = vi.fn();
    const onCancel = vi.fn();
    render(
      <MessageEditBox initialText={SEED} onSave={onSave} onCancel={onCancel} />,
    );
    const box = screen.getByLabelText("Edit message");

    fireEvent.keyDown(box, { key: "Enter" });
    expect(onSave).toHaveBeenCalledWith(SEED);

    fireEvent.keyDown(box, { key: "Escape" });
    expect(onCancel).toHaveBeenCalled();
  });

  it("Shift+Enter inserts a newline rather than saving", () => {
    const onSave = vi.fn();
    render(
      <MessageEditBox initialText={SEED} onSave={onSave} onCancel={() => {}} />,
    );
    fireEvent.keyDown(screen.getByLabelText("Edit message"), {
      key: "Enter",
      shiftKey: true,
    });
    expect(onSave).not.toHaveBeenCalled();
  });
});
