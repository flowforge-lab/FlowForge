import { beforeEach, describe, expect, it } from "vitest";

import { ipc } from "@/lib/ipc";
import { usePermissionMatrixStore } from "@/store/permission-matrix";

describe("usePermissionMatrixStore", () => {
  beforeEach(async () => {
    usePermissionMatrixStore.setState({
      matrix: null,
      overrides: [],
      loading: true,
      saving: false,
      error: null,
    });
    // Clear any overrides left by a prior test (MockIpc is a shared singleton).
    const view = await ipc.getPermissionMatrix();
    for (const { tool } of view.overrides) await ipc.removeToolOverride(tool);
  });

  it("loads the matrix and (empty) overrides from IPC", async () => {
    await usePermissionMatrixStore.getState().load();
    const { matrix, overrides } = usePermissionMatrixStore.getState();
    expect(matrix?.auto.readonly).toBe("allow");
    expect(matrix?.auto.dangerous).toBe("deny");
    expect(overrides).toEqual([]);
  });

  it("setCell persists a single cell and reconciles with the view", async () => {
    await usePermissionMatrixStore.getState().load();
    await usePermissionMatrixStore.getState().setCell("plan", "write", "ask");
    expect(usePermissionMatrixStore.getState().matrix?.plan.write).toBe("ask");

    await usePermissionMatrixStore.getState().load();
    expect(usePermissionMatrixStore.getState().matrix?.plan.write).toBe("ask");
  });

  it("setOverride adds a sorted per-tool override", async () => {
    await usePermissionMatrixStore.getState().load();
    await usePermissionMatrixStore.getState().setOverride("web_fetch", "deny");
    await usePermissionMatrixStore.getState().setOverride("aws", "ask");

    const { overrides } = usePermissionMatrixStore.getState();
    expect(overrides).toEqual([
      { tool: "aws", cell: "ask" },
      { tool: "web_fetch", cell: "deny" },
    ]);
  });

  it("removeOverride drops the entry", async () => {
    await usePermissionMatrixStore.getState().load();
    await usePermissionMatrixStore.getState().setOverride("web_fetch", "deny");
    await usePermissionMatrixStore.getState().removeOverride("web_fetch");
    expect(usePermissionMatrixStore.getState().overrides).toEqual([]);
  });
});
