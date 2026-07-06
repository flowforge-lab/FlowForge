import { beforeEach, describe, expect, it } from "vitest";

import { useFindStore } from "@/store/find";

beforeEach(() => {
  useFindStore.setState({
    open: false,
    sessionId: null,
    seedQuery: null,
    seedMessageId: null,
  });
});

describe("useFindStore", () => {
  it("opens for a session with no seed by default", () => {
    useFindStore.getState().openFind("s1");
    const s = useFindStore.getState();
    expect(s.open).toBe(true);
    expect(s.sessionId).toBe("s1");
    expect(s.seedQuery).toBeNull();
    expect(s.seedMessageId).toBeNull();
  });

  it("carries a global-search seed (query + messageId)", () => {
    useFindStore
      .getState()
      .openFind("s1", { query: "parser", messageId: "m9" });
    const s = useFindStore.getState();
    expect(s).toMatchObject({
      open: true,
      sessionId: "s1",
      seedQuery: "parser",
      seedMessageId: "m9",
    });
  });

  it("consumeSeed clears the seed but leaves the bar open", () => {
    useFindStore.getState().openFind("s1", { query: "x", messageId: "m1" });
    useFindStore.getState().consumeSeed();
    const s = useFindStore.getState();
    expect(s.open).toBe(true);
    expect(s.seedQuery).toBeNull();
    expect(s.seedMessageId).toBeNull();
  });

  it("toggleFind and closeFind drop any standing seed", () => {
    useFindStore.getState().openFind("s1", { query: "x", messageId: "m1" });
    useFindStore.getState().toggleFind("s1"); // same session → closes
    expect(useFindStore.getState().open).toBe(false);
    expect(useFindStore.getState().seedQuery).toBeNull();

    useFindStore.getState().openFind("s1", { query: "y", messageId: "m2" });
    useFindStore.getState().closeFind();
    expect(useFindStore.getState().seedMessageId).toBeNull();
  });
});
