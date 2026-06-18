import { describe, expect, it } from "vitest";

import { formatUpdateStatus, type UpdateStatus } from "./about";

describe("formatUpdateStatus", () => {
  it("reports the latest version when up to date", () => {
    const status: UpdateStatus = { kind: "upToDate", version: "0.1.0" };
    expect(formatUpdateStatus(status)).toBe("You're on the latest version.");
  });

  it("names the available version when an update exists", () => {
    const status: UpdateStatus = {
      kind: "available",
      version: "0.2.0",
      notes: null,
    };
    expect(formatUpdateStatus(status)).toBe("Version 0.2.0 is available.");
  });
});
