import { beforeEach, describe, expect, it } from "vitest";

import { ipc } from "@/lib/ipc";
import { useSearchConfigStore } from "@/store/search-config";

describe("useSearchConfigStore", () => {
  beforeEach(async () => {
    useSearchConfigStore.setState({
      config: null,
      loading: false,
      saving: false,
      error: null,
    });
    await ipc.setSearchConfig("searxNg", undefined);
  });

  it("loads persisted search config from IPC", async () => {
    await ipc.setSearchConfig("searxNg", "https://searx.example.org");

    await useSearchConfigStore.getState().load();
    expect(useSearchConfigStore.getState().config?.baseUrl).toBe(
      "https://searx.example.org",
    );
  });

  it("persists backend switches and clears base URL for hosted backends", async () => {
    await ipc.setSearchConfig("searxNg", "https://searx.example.org");
    await useSearchConfigStore.getState().load();

    await useSearchConfigStore.getState().setBackend("brave");

    const cfg = useSearchConfigStore.getState().config;
    expect(cfg?.backend).toBe("brave");
    expect(cfg?.baseUrl).toBeUndefined();
    expect(cfg?.hasKey).toBe(false);
  });

  it("persists SearXNG base URL updates", async () => {
    await useSearchConfigStore.getState().load();
    await useSearchConfigStore
      .getState()
      .setBaseUrl("https://search.example.net");

    expect(useSearchConfigStore.getState().config?.baseUrl).toBe(
      "https://search.example.net",
    );
  });
});
