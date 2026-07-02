// @vitest-environment jsdom

import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MessageHeader } from "@/components/message-header";
import { usePrefsStore } from "@/store/prefs";
import { useProfilesStore } from "@/store/profiles";
import { formatMessageTime } from "@/lib/format-message-time";
import type { Profile } from "@/store/profiles";

const CREATED_AT = 1_700_000_000_000; // fixed epoch ms

const profile = (over: Partial<Profile>): Profile => ({
  id: "codon",
  name: "Codon",
  description: "",
  skillCount: 0,
  locked: false,
  accent: "teal" as Profile["accent"],
  ...over,
});

afterEach(() => {
  cleanup();
  usePrefsStore.setState({ displayName: "" });
  useProfilesStore.setState({ profiles: [], activeId: "default" });
});

describe("MessageHeader", () => {
  it('falls back to "You" for a user message with a blank display name', () => {
    usePrefsStore.setState({ displayName: "" });
    render(<MessageHeader role="user" createdAt={CREATED_AT} />);
    expect(screen.getByText("You")).toBeTruthy();
    expect(screen.getByText(formatMessageTime(CREATED_AT))).toBeTruthy();
  });

  it("shows the configured display name for a user message", () => {
    usePrefsStore.setState({ displayName: "Abid" });
    render(<MessageHeader role="user" createdAt={CREATED_AT} />);
    expect(screen.getByText("Abid")).toBeTruthy();
  });

  it("shows the active phenotype's name for an assistant message", () => {
    useProfilesStore.setState({
      activeId: "data-science",
      profiles: [profile({ id: "data-science", name: "Data Science" })],
    });
    render(<MessageHeader role="assistant" createdAt={CREATED_AT} />);
    expect(screen.getByText("Data Science")).toBeTruthy();
  });

  it('falls back to "Assistant" when no profile matches the active id', () => {
    // The reset state (activeId "default", no profiles) must not surface the raw
    // id slug as the author name.
    useProfilesStore.setState({ activeId: "default", profiles: [] });
    render(<MessageHeader role="assistant" createdAt={CREATED_AT} />);
    expect(screen.getByText("Assistant")).toBeTruthy();
    expect(screen.queryByText("default")).toBeNull();
  });

  it("omits the time when createdAt has no usable timestamp", () => {
    usePrefsStore.setState({ displayName: "Abid" });
    render(<MessageHeader role="user" createdAt={0} />);
    expect(screen.getByText("Abid")).toBeTruthy();
    // Only the name span renders — no clock time.
    expect(screen.queryByText(/\d{1,2}:\d{2}/)).toBeNull();
  });
});
