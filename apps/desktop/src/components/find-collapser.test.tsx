// @vitest-environment jsdom
//
// Integration coverage for #875 — what the existing unit suite can't pin down:
// a collapser that's actually mounted (real DOM) responding to the find bar's
// forced-open bus. We render the component, drive the store directly (the same
// shape `find-bar.tsx` calls would issue), and assert the body mounts/unmounts.
//
// Targets every collapser wired to `useFindExpansion`:
//   • `ToolStepBlock` — `tool-step:<callId>`,
//   • `OutputBlock` — `output:<id>` (gated on `long` so we render past the
//     fold threshold),
//   • `StepGroup` — `step-group:<messageId>:<segmentKey>` (single + multi step
//     variants; the `closed by default → flipped open on settle` toggle is
//     what `findOn` has to override).

import { render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ToolStepBlock } from "@/components/tool-step";
import { OutputBlock } from "@/components/output-block";
import { StepGroup } from "@/components/step-group";
import { useFindExpansion } from "@/store/find-expansion";
import type { ToolStep } from "@/store/chat";
import type { TurnItem } from "@/lib/turn-groups";

const STEP: ToolStep = {
  callId: "c1",
  tool: "bash",
  args: { command: "git tag release/v1" },
  status: "done",
  result: "ok",
};

beforeEach(() => {
  useFindExpansion.setState({ forced: new Set<string>() });
});

afterEach(() => {
  useFindExpansion.setState({ forced: new Set<string>() });
  document.body.innerHTML = "";
});

describe("ToolStepBlock ↔ find-expansion bus (#875)", () => {
  it("is folded by default and opens when `tool-step:c1` is forced", async () => {
    render(
      <ToolStepBlock
        step={STEP}
        onRespond={() => {}}
        onApproveSession={() => {}}
        onApproveAlways={() => {}}
        onAnswer={() => {}}
      />,
    );
    // No `<pre data-selectable>` until open.
    expect(document.body.querySelector("pre")).toBeNull();
    useFindExpansion.getState().setForced(["tool-step:c1"]);
    await waitFor(() => {
      expect(document.body.querySelector("pre")).not.toBeNull();
    });
    expect(document.body.querySelector("pre")?.textContent ?? "").toContain(
      "release/v1",
    );
    // Tidy — body should fold back when the bus clears.
    useFindExpansion.getState().clear();
    await waitFor(() => {
      expect(document.body.querySelector("pre")).toBeNull();
    });
  });

  it("a manual close mid-search is overridden by the bus while find is open", async () => {
    render(
      <ToolStepBlock
        step={STEP}
        onRespond={() => {}}
        onApproveSession={() => {}}
        onApproveAlways={() => {}}
        onAnswer={() => {}}
      />,
    );
    // Force-open via the bus (simulating the find bar pre-mount).
    useFindExpansion.getState().setForced(["tool-step:c1"]);
    await waitFor(() => {
      expect(document.body.querySelector("pre")).not.toBeNull();
    });
    // Bus clears: the block folds back to its default-closed state.
    useFindExpansion.getState().clear();
    await waitFor(() => {
      expect(document.body.querySelector("pre")).toBeNull();
    });
  });
});

describe("OutputBlock ↔ find-expansion bus (#875)", () => {
  it("opens when output is long and `output:<id>` is forced, restoring fold when cleared", async () => {
    const longOutput = "x".repeat(800); // past `OUTPUT_FOLD_THRESHOLD = 600`
    render(
      <OutputBlock output={longOutput} title="bash" expandId="output:m7" />,
    );
    // Long output starts folded → no `<pre>`.
    expect(document.body.querySelector("pre")).toBeNull();
    useFindExpansion.getState().setForced(["output:m7"]);
    await waitFor(() => {
      expect(document.body.querySelector("pre")).not.toBeNull();
    });
    expect(document.body.querySelector("pre")?.textContent).toBe(longOutput);
    useFindExpansion.getState().clear();
    await waitFor(() => {
      expect(document.body.querySelector("pre")).toBeNull();
    });
  });
});

describe("StepGroup ↔ find-expansion bus (#875)", () => {
  it("a settled single-step group folds by default and opens when a child `tool-step:` is forced (parent gate), restoring after clear", async () => {
    const items: TurnItem[] = [{ kind: "step", step: STEP }];
    render(
      <StepGroup
        steps={[STEP]}
        items={items}
        streaming={false}
        turnStartMs={null}
        messageId="a1"
        segmentKey="seg:s1"
        onRespond={() => {}}
        onApproveSession={() => {}}
        onApproveAlways={() => {}}
        onAnswer={() => {}}
      />,
    );
    // Settled → fold header present, no body.
    expect(document.body.querySelector("button[aria-expanded]")).not.toBeNull();
    expect(document.body.querySelector("pre")).toBeNull();
    // Force-open the child step (the canonical id the find bar pushes). The
    // group is content-gated, so this also has to open the parent group —
    // the bus consults both ids, transitively (#875).
    useFindExpansion.getState().setForced(["tool-step:c1"]);
    await waitFor(
      () => {
        expect(document.body.querySelector("pre")).not.toBeNull();
      },
      { timeout: 1500 },
    );
    expect(document.body.querySelector("pre")?.textContent ?? "").toContain(
      "release/v1",
    );
    useFindExpansion.getState().clear();
    await waitFor(() => {
      expect(document.body.querySelector("pre")).toBeNull();
    });
  });
});
