// @vitest-environment jsdom

import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MessageHeader } from "@/components/message-header";
import { usePrefsStore } from "@/store/prefs";
import { useProfilesStore } from "@/store/profiles";
import { useEditedMessagesStore } from "@/store/edited-messages";
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
  useEditedMessagesStore.setState({ editedIds: [] });
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

  it("prefers the persisted author over the active phenotype (#657)", () => {
    // Active phenotype is Data Science, but this message was authored by Codon --
    // the header must show the historical author, not the current active one.
    useProfilesStore.setState({
      activeId: "data-science",
      profiles: [
        profile({ id: "data-science", name: "Data Science" }),
        profile({ id: "codon", name: "Codon" }),
      ],
    });
    render(
      <MessageHeader
        role="assistant"
        createdAt={CREATED_AT}
        authorName="codon"
      />,
    );
    expect(screen.getByText("Codon")).toBeTruthy();
    expect(screen.queryByText("Data Science")).toBeNull();
  });

  it("title-cases a persisted author whose profile no longer exists", () => {
    // The authoring phenotype was deleted, so it is not in the list; fall back to
    // a title-cased form of the raw name rather than the wrong active phenotype.
    useProfilesStore.setState({
      activeId: "data-science",
      profiles: [profile({ id: "data-science", name: "Data Science" })],
    });
    render(
      <MessageHeader
        role="assistant"
        createdAt={CREATED_AT}
        authorName="legacy-bot"
      />,
    );
    expect(screen.getByText("Legacy Bot")).toBeTruthy();
  });

  it("falls back to the active phenotype when no author is persisted", () => {
    // Pre-#657 rows have no stored author; live resolution still applies.
    useProfilesStore.setState({
      activeId: "data-science",
      profiles: [profile({ id: "data-science", name: "Data Science" })],
    });
    render(<MessageHeader role="assistant" createdAt={CREATED_AT} />);
    expect(screen.getByText("Data Science")).toBeTruthy();
  });

  it("omits the time when createdAt has no usable timestamp", () => {
    usePrefsStore.setState({ displayName: "Abid" });
    render(<MessageHeader role="user" createdAt={0} />);
    expect(screen.getByText("Abid")).toBeTruthy();
    // Only the name span renders — no clock time.
    expect(screen.queryByText(/\d{1,2}:\d{2}/)).toBeNull();
  });

  describe('"edited" hint (#929 B)', () => {
    it("renders nothing for a message that was never edited", () => {
      render(
        <MessageHeader role="user" createdAt={CREATED_AT} messageId="m1" />,
      );
      expect(screen.queryByText("edited")).toBeNull();
    });

    it("renders for a marked message", () => {
      useEditedMessagesStore.getState().markEdited("m1");
      render(
        <MessageHeader role="user" createdAt={CREATED_AT} messageId="m1" />,
      );
      expect(screen.getByText("edited")).toBeTruthy();
    });

    it("is a static label, not an interactive affordance", () => {
      // FlowForge truncates on edit, so there is no original to open. A clickable
      // badge would promise a version history that does not exist.
      useEditedMessagesStore.getState().markEdited("m1");
      render(
        <MessageHeader role="user" createdAt={CREATED_AT} messageId="m1" />,
      );
      const label = screen.getByText("edited");
      expect(label.tagName).toBe("SPAN");
      expect(screen.queryByRole("button", { name: /edited/i })).toBeNull();
      expect(screen.queryByRole("link", { name: /edited/i })).toBeNull();
      expect(label.getAttribute("role")).toBeNull();
      expect(label.className).not.toMatch(/cursor-pointer|underline/);
    });
  });
});
