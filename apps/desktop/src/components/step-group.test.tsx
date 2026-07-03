// @vitest-environment jsdom

import { render, fireEvent, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { StepGroup } from "@/components/step-group";
import type { ToolStep } from "@/store/chat";
import type { TurnItem } from "@/lib/turn-groups";

const noop = () => {};

function step(partial: Partial<ToolStep>): ToolStep {
  return {
    callId: "c1",
    tool: "bash",
    args: { command: "ls" },
    status: "done",
    ...partial,
  };
}

function renderGroup(props: { streaming?: boolean; answer?: string }) {
  const steps = [
    step({ callId: "c1", args: { command: "ls" } }),
    step({ callId: "c2", args: { command: "pwd" } }),
  ];
  return render(
    <StepGroup
      steps={steps}
      streaming={props.streaming ?? false}
      answer={props.answer}
      onRespond={noop}
      onApproveSession={noop}
      onApproveAlways={noop}
      onAnswer={noop}
    />,
  );
}

/** The fold header — the only element carrying aria-expanded. */
function header(container: HTMLElement): HTMLElement {
  return container.querySelector("button[aria-expanded]")!;
}

describe("StepGroup collapsed answer preview (#414)", () => {
  const ANSWER = "Here is the final answer to your request.";

  it("shows a muted 2-line preview of the answer when collapsed (settled)", () => {
    const { container } = renderGroup({ answer: ANSWER });
    // Settled + untouched → collapsed by default.
    expect(header(container).getAttribute("aria-expanded")).toBe("false");
    const preview = within(container).getByText(ANSWER);
    expect(preview).toBeTruthy();
    expect(preview.className).toContain("line-clamp-2");
    // Steps are not rendered while collapsed.
    expect(within(container).queryByText("Run `ls`")).toBeNull();
  });

  it("hides the preview and reveals the steps once expanded", () => {
    const { container } = renderGroup({ answer: ANSWER });
    fireEvent.click(header(container));
    expect(header(container).getAttribute("aria-expanded")).toBe("true");
    expect(within(container).queryByText(ANSWER)).toBeNull();
    expect(within(container).getByText("Run `ls`")).toBeTruthy();
    expect(within(container).getByText("Run `pwd`")).toBeTruthy();
  });

  it("strips markdown syntax from the preview", () => {
    const { container } = renderGroup({
      answer: "### Heading\n\nThis is **bold** prose.",
    });
    const preview = container.querySelector("p.line-clamp-2")!;
    expect(preview.textContent).toBe("Heading This is bold prose.");
  });

  it("renders no preview when the turn has no answer text", () => {
    const { container } = renderGroup({ answer: "" });
    expect(header(container).getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector("p.line-clamp-2")).toBeNull();
  });

  it("renders no preview while streaming (group is expanded)", () => {
    const { container } = renderGroup({ streaming: true, answer: ANSWER });
    expect(header(container).getAttribute("aria-expanded")).toBe("true");
    expect(within(container).queryByText(ANSWER)).toBeNull();
  });
});

describe("StepGroup folded thought (#687)", () => {
  it("renders a short/operational thought as a compact row inside the group", () => {
    const steps = [step({ callId: "c1", args: { command: "ls" } })];
    const items: TurnItem[] = [
      { kind: "thought", text: "Now let me verify the helper.", key: "a1" },
      { kind: "step", step: steps[0] },
    ];
    // streaming keeps the group expanded so the folded rows are visible.
    const { container } = render(
      <StepGroup
        steps={steps}
        items={items}
        streaming
        hasAnswer={false}
        onRespond={noop}
        onApproveSession={noop}
        onApproveAlways={noop}
        onAnswer={noop}
      />,
    );

    // A compact "Thought" row renders (collapsed by default) with a truncated preview.
    const label = within(container).getByText("Thought");
    const row = label.closest("button")!;
    expect(row.getAttribute("aria-expanded")).toBe("false");
    expect(within(row).getByText(/Now let me verify the helper/)).toBeTruthy();

    // The thought sits before the step row it narrates, inside the group.
    const firstStep = within(container).getByText("Run `ls`");
    expect(
      label.compareDocumentPosition(firstStep) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    // It expands to reveal the full text.
    fireEvent.click(row);
    expect(row.getAttribute("aria-expanded")).toBe("true");
  });
});

describe("StepGroup interleaved reasoning (#574)", () => {
  it("renders a reasoning row in position, before its sibling step rows", () => {
    const steps = [
      step({ callId: "c1", args: { command: "ls" } }),
      step({ callId: "c2", args: { command: "pwd" } }),
    ];
    const items: TurnItem[] = [
      { kind: "reasoning", text: "plan: list then locate", key: "a1" },
      { kind: "step", step: steps[0] },
      { kind: "step", step: steps[1] },
    ];
    // streaming keeps the group expanded so every row is visible.
    const { container } = render(
      <StepGroup
        steps={steps}
        items={items}
        streaming
        hasAnswer={false}
        onRespond={noop}
        onApproveSession={noop}
        onApproveAlways={noop}
        onAnswer={noop}
      />,
    );

    // Exactly one Thinking block — no top-pinned duplicate above the steps.
    const thinking = within(container).getAllByText("Thinking");
    expect(thinking).toHaveLength(1);

    // The reasoning row precedes the first step row in document order.
    const reasoning = thinking[0];
    const firstStep = within(container).getByText("Run `ls`");
    expect(
      reasoning.compareDocumentPosition(firstStep) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });
});
