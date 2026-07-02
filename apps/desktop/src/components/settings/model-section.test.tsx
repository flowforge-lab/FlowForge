// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ModelSection } from "@/components/settings/model-section";
import { activeConnection, useModelConfigStore } from "@/store/model-config";
import { useSettingsStore } from "@/store/settings";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

// jsdom lacks ResizeObserver, which radix's Slider (summarization threshold)
// measures with on mount.
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
(globalThis as { ResizeObserver?: unknown }).ResizeObserver =
  ResizeObserverStub;

let container: HTMLDivElement;
let root: Root;

/** Render and let the mount-time `load()` (and any queued microtasks) settle. */
async function renderSection() {
  await act(async () => {
    root.render(<ModelSection />);
    await flush();
  });
}

function flush() {
  return new Promise((r) => setTimeout(r, 0));
}

async function click(el: Element | null | undefined) {
  await act(async () => {
    el?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
  });
}

/** First element under `scope` whose trimmed text equals `text`. */
function byText(
  text: string,
  selector = "*",
  scope: ParentNode = container,
): HTMLElement | null {
  return (
    [...scope.querySelectorAll<HTMLElement>(selector)].find(
      (el) => el.textContent?.trim() === text,
    ) ?? null
  );
}

/** The accordion header button for a provider (carries aria-expanded). */
function cardHeader(name: string): HTMLElement | null {
  return (
    [...container.querySelectorAll<HTMLElement>("button[aria-expanded]")].find(
      (el) => el.textContent?.includes(name),
    ) ?? null
  );
}

/** The card element wrapping a provider — scope to it since several cards (e.g.
 *  the active candle-vLLM) are open at once and share button/label text. */
function card(name: string): HTMLElement {
  const header = cardHeader(name);
  if (!header?.parentElement) throw new Error(`no card for ${name}`);
  return header.parentElement;
}

function hasLabel(text: string, scope: ParentNode = container): boolean {
  return [...scope.querySelectorAll("label")].some(
    (el) => el.textContent?.trim() === text,
  );
}

/** The "Reasoning effort" segmented control group. */
function effortGroup(): HTMLElement {
  const g = container.querySelector('[aria-label="Reasoning effort"]');
  if (!g) throw new Error("no reasoning-effort group");
  return g as HTMLElement;
}

/** Currently-selected effort label (the `data-state="on"` segment). */
function selectedEffort(): string | undefined {
  return effortGroup().querySelector('[data-state="on"]')?.textContent?.trim();
}

beforeEach(async () => {
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
  useSettingsStore.setState({ activeSection: "model", resetHandler: null });
  // Reset to backend truth and drop any secret left by a prior test (all three
  // Bedrock kinds, so a future IAM-keys test can't bleed hasKey into later ones).
  await useModelConfigStore.getState().load();
  for (const kind of ["apiKey", "secretAccessKey", "sessionToken"] as const) {
    await useModelConfigStore.getState().clearSecret("bedrock", kind);
  }
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  localStorage.clear();
});

describe("ModelSection provider accordion", () => {
  it("renders a card per registry connection, including Bedrock, SiliconFlow, and OpenAI", async () => {
    await renderSection();
    expect(cardHeader("candle-vLLM")).not.toBeNull();
    expect(cardHeader("Ollama")).not.toBeNull();
    expect(cardHeader("AWS Bedrock")).not.toBeNull();
    expect(cardHeader("SiliconFlow")).not.toBeNull();
    expect(cardHeader("OpenAI")).not.toBeNull();
  });

  it("re-syncs Bedrock form fields when the connection is updated in place (no remount)", async () => {
    await renderSection();
    await click(cardHeader("AWS Bedrock"));
    const regionInput = () =>
      container.querySelector<HTMLInputElement>("#region-bedrock");
    const profileInput = () =>
      container.querySelector<HTMLInputElement>("#profile-bedrock");
    // Mount-time seed values (from the mock registry).
    expect(regionInput()?.value).toBe("us-east-1");
    expect(profileInput()?.value).toBe("bedrock-profile");

    // Update the same connection (id unchanged, so the card is NOT remounted).
    await act(async () => {
      const reg = useModelConfigStore.getState().registry!;
      useModelConfigStore.setState({
        registry: {
          ...reg,
          connections: reg.connections.map((c) =>
            c.id === "bedrock"
              ? {
                  ...c,
                  region: "ap-southeast-2",
                  authMode: "profile" as const,
                  awsProfile: "team-profile",
                }
              : c,
          ),
        },
      });
      await flush();
    });

    // The form reflects the updated connection rather than the stale mount-time
    // seeds, so a later Save can't persist defaults over the real config (#508).
    expect(regionInput()?.value).toBe("ap-southeast-2");
    expect(profileInput()?.value).toBe("team-profile");
  });

  it("shows the hosted-key fields (Base URL + API Key) for SiliconFlow", async () => {
    await renderSection();
    await click(cardHeader("SiliconFlow"));
    const sf = card("SiliconFlow");
    expect(hasLabel("Base URL", sf)).toBe(true);
    expect(hasLabel("API Key", sf)).toBe(true);
    // Hosted, not the local "Host (optional)" or Bedrock region form.
    expect(hasLabel("Host (optional)", sf)).toBe(false);
    expect(hasLabel("Region", sf)).toBe(false);
  });

  it("surfaces a Test Connection error for SiliconFlow with no key", async () => {
    await renderSection();
    await click(cardHeader("SiliconFlow"));
    const sf = card("SiliconFlow");
    await click(byText("Test Connection", "button", sf));
    const alert = card("SiliconFlow").querySelector('[role="alert"]');
    expect(alert?.textContent).toMatch(/SiliconFlow API key/i);
  });

  it("shows the hosted-key fields (Base URL + API Key) for OpenAI", async () => {
    await renderSection();
    await click(cardHeader("OpenAI"));
    const openai = card("OpenAI");
    expect(hasLabel("Base URL", openai)).toBe(true);
    expect(hasLabel("API Key", openai)).toBe(true);
    expect(hasLabel("Host (optional)", openai)).toBe(false);
    expect(hasLabel("Region", openai)).toBe(false);
  });

  it("surfaces a Test Connection error for OpenAI with no key", async () => {
    await renderSection();
    await click(cardHeader("OpenAI"));
    const openai = card("OpenAI");
    await click(byText("Test Connection", "button", openai));
    const alert = card("OpenAI").querySelector('[role="alert"]');
    expect(alert?.textContent).toMatch(/OpenAI API key/i);
  });

  it("disables the Thinking switch when a hosted OpenAI-compatible connection is active", async () => {
    await renderSection();
    // candle-vLLM (active by default) supports the thinking flag.
    const before = container.querySelector(
      'button[aria-label="Thinking"]',
    ) as HTMLButtonElement;
    expect(before.disabled).toBe(false);

    await act(async () => {
      await useModelConfigStore.getState().setActiveConnection("openai");
      await flush();
    });

    const after = container.querySelector(
      'button[aria-label="Thinking"]',
    ) as HTMLButtonElement;
    expect(after.disabled).toBe(true);
    expect(
      byText(
        "This provider doesn't support toggling reasoning; it's shown when the model emits it.",
      ),
    ).not.toBeNull();
  });

  it("shows the Warmup switch for local kinds and toggles it, hides it for hosted (#61)", async () => {
    await renderSection();
    // The MockIpc singleton persists edits across tests, so pin a known start:
    // a local connection with warmup enabled.
    await act(async () => {
      await useModelConfigStore
        .getState()
        .setWarmupEnabled("candle-vllm", true);
      await useModelConfigStore.getState().setActiveConnection("candle-vllm");
      await flush();
    });
    const warmup = container.querySelector(
      'button[aria-label="Warmup"]',
    ) as HTMLButtonElement | null;
    expect(warmup).not.toBeNull();

    await click(warmup!);
    expect(
      activeConnection(useModelConfigStore.getState().registry)?.warmupEnabled,
    ).toBe(false);

    // Hosted kinds have no warmup nudge -> the control is not rendered.
    await act(async () => {
      await useModelConfigStore.getState().setActiveConnection("openai");
      await flush();
    });
    expect(container.querySelector('button[aria-label="Warmup"]')).toBeNull();
  });

  it("shows the Context window select for Ollama, persists a preset, and hides it for other kinds (#651)", async () => {
    await renderSection();
    await act(async () => {
      await useModelConfigStore.getState().setNumCtx("ollama", null);
      await useModelConfigStore.getState().setActiveConnection("ollama");
      await flush();
    });
    const trigger = container.querySelector(
      'button[aria-label="Context window"]',
    );
    expect(trigger).not.toBeNull();
    // Auto = the field is left unset (omitted on the wire).
    expect(
      activeConnection(useModelConfigStore.getState().registry)?.numCtx,
    ).toBeUndefined();

    // A chosen preset persists onto the active connection.
    await act(async () => {
      await useModelConfigStore.getState().setNumCtx("ollama", 8192);
      await flush();
    });
    expect(
      activeConnection(useModelConfigStore.getState().registry)?.numCtx,
    ).toBe(8192);

    // Clearing back to Auto drops the field again.
    await act(async () => {
      await useModelConfigStore.getState().setNumCtx("ollama", null);
      await flush();
    });
    expect(
      activeConnection(useModelConfigStore.getState().registry)?.numCtx,
    ).toBeUndefined();

    // candle-vLLM (the other local kind) serves its window at launch, so the
    // control is not rendered for it (#311: no dead controls).
    await act(async () => {
      await useModelConfigStore.getState().setActiveConnection("candle-vllm");
      await flush();
    });
    expect(
      container.querySelector('button[aria-label="Context window"]'),
    ).toBeNull();

    // The MockIpc singleton persists the active pointer across tests; leave a
    // thinking-enabled connection active so later tests don't inherit a local
    // kind (whose reasoning controls are disabled).
    await act(async () => {
      await useModelConfigStore.getState().setActiveConnection("openai");
      await flush();
    });
  });

  // The module-level MockIpc singleton persists the active pointer and connection
  // edits across tests, so each effort test establishes its own starting state
  // rather than relying on the seed defaults.
  it("reflects the active connection's reasoning effort and updates it on click", async () => {
    await renderSection();
    const id = activeConnection(useModelConfigStore.getState().registry)!.id;
    await act(async () => {
      await useModelConfigStore.getState().setEffort(id, "low");
      await flush();
    });
    expect(selectedEffort()).toBe("Low");

    await click(byText("High", "button", effortGroup()));
    expect(selectedEffort()).toBe("High");
    // Persisted onto the active connection (per-connection field, #395).
    expect(
      activeConnection(useModelConfigStore.getState().registry)
        ?.reasoningEffort,
    ).toBe("high");
  });

  it("shows each connection's own effort when the active one changes", async () => {
    await renderSection();
    await act(async () => {
      const { setEffort } = useModelConfigStore.getState();
      await setEffort("candle-vllm", "medium");
      await setEffort("bedrock", "low");
      await useModelConfigStore.getState().setActiveConnection("candle-vllm");
      await flush();
    });
    expect(selectedEffort()).toBe("Medium");

    await act(async () => {
      await useModelConfigStore.getState().setActiveConnection("bedrock");
      await flush();
    });
    // The dial follows the active connection's own effort.
    expect(selectedEffort()).toBe("Low");
  });

  it("disables the effort dial when Thinking is off", async () => {
    await renderSection();
    // candle-vLLM supports the thinking toggle; make it active with thinking on.
    await act(async () => {
      await useModelConfigStore.getState().setActiveConnection("candle-vllm");
      await useModelConfigStore.getState().setThinking("candle-vllm", true);
      await flush();
    });
    const high = byText("High", "button", effortGroup()) as HTMLButtonElement;
    expect(high.disabled).toBe(false);

    await act(async () => {
      await useModelConfigStore.getState().setThinking("candle-vllm", false);
      await flush();
    });
    const highOff = byText(
      "High",
      "button",
      effortGroup(),
    ) as HTMLButtonElement;
    expect(highOff.disabled).toBe(true);
  });

  it("switches Bedrock auth mode, revealing the matching fields", async () => {
    await renderSection();
    await click(cardHeader("AWS Bedrock"));
    const bedrock = card("AWS Bedrock");
    // Profile mode by default.
    expect(hasLabel("AWS Profile", bedrock)).toBe(true);
    expect(hasLabel("Bedrock API Key", bedrock)).toBe(false);

    await click(byText("IAM Keys", "button", bedrock));
    expect(hasLabel("Access Key ID", bedrock)).toBe(true);
    expect(hasLabel("Secret Access Key", bedrock)).toBe(true);

    await click(byText("API Key", "button", bedrock));
    expect(hasLabel("Bedrock API Key", bedrock)).toBe(true);
    expect(hasLabel("AWS Profile", bedrock)).toBe(false);
  });

  it("surfaces a Test Connection error for API-Key mode with no key", async () => {
    await renderSection();
    await click(cardHeader("AWS Bedrock"));
    const bedrock = card("AWS Bedrock");
    await click(byText("API Key", "button", bedrock));
    // Persist the auth mode so the probe sees it, then test (no key stored).
    await click(byText("Save", "button", bedrock));
    await click(byText("Test Connection", "button", bedrock));
    const alert = card("AWS Bedrock").querySelector('[role="alert"]');
    expect(alert?.textContent).toMatch(/API key/i);
  });
});
