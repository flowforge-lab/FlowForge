import { beforeEach, describe, expect, it } from "vitest";

import { usePhenoMcpNoticeStore } from "./pheno-mcp-notice";

beforeEach(() => {
  usePhenoMcpNoticeStore.setState({ notice: null, seq: 0 });
});

describe("usePhenoMcpNoticeStore", () => {
  it("starts empty", () => {
    const s = usePhenoMcpNoticeStore.getState();
    expect(s.notice).toBeNull();
    expect(s.seq).toBe(0);
  });

  it("show stores the notice and bumps seq", () => {
    const notice = { phenotype: "codon", servers: ["codegraph"] };
    usePhenoMcpNoticeStore.getState().show(notice);
    const s = usePhenoMcpNoticeStore.getState();
    expect(s.notice).toEqual(notice);
    expect(s.seq).toBe(1);
  });

  it("bumps seq again when one notice replaces another (timer re-arm)", () => {
    const { show } = usePhenoMcpNoticeStore.getState();
    show({ phenotype: "codon", servers: ["codegraph"] });
    show({ phenotype: "codon", servers: ["codegraph"] }); // same content
    expect(usePhenoMcpNoticeStore.getState().seq).toBe(2);
  });

  it("dismiss clears the notice", () => {
    const { show, dismiss } = usePhenoMcpNoticeStore.getState();
    show({ phenotype: "codon", servers: ["codegraph"] });
    dismiss();
    expect(usePhenoMcpNoticeStore.getState().notice).toBeNull();
  });
});
