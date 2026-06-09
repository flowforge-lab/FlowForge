import { useEffect, useRef, useState } from "react";
import { ipc } from "./lib/ipc";
import type { Message, Session } from "./bindings";
import "./App.css";

// Minimal chat shell proving the IPC contract end-to-end. Abid owns the real UI
// (command palette, theming, flow canvas) — this is just a working seam to build on.
function App() {
  const [session, setSession] = useState<Session | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const streamId = useRef<string | null>(null);

  useEffect(() => {
    ipc.createSession().then((s) => setSession(s));
  }, []);

  useEffect(() => {
    const unlisteners: Promise<() => void>[] = [];

    unlisteners.push(
      ipc.onToken((e) => {
        setMessages((prev) => {
          const next = [...prev];
          const idx = next.findIndex((m) => m.id === e.messageId);
          if (idx >= 0) {
            next[idx] = { ...next[idx], content: next[idx].content + e.delta };
          } else {
            next.push({
              id: e.messageId,
              sessionId: e.sessionId,
              role: "assistant",
              content: e.delta,
              createdAt: Date.now(),
            });
          }
          return next;
        });
      }),
    );

    unlisteners.push(
      ipc.onTurnDone(() => {
        setStreaming(false);
        streamId.current = null;
      }),
    );

    unlisteners.push(
      ipc.onTurnError((e) => {
        setStreaming(false);
        setMessages((prev) => [
          ...prev,
          {
            id: crypto.randomUUID(),
            sessionId: e.sessionId,
            role: "system",
            content: `Error: ${e.message}`,
            createdAt: Date.now(),
          },
        ]);
      }),
    );

    return () => {
      unlisteners.forEach((p) => p.then((un) => un()));
    };
  }, []);

  async function send() {
    if (!session || !input.trim() || streaming) return;
    const content = input.trim();
    setInput("");
    setMessages((prev) => [
      ...prev,
      {
        id: crypto.randomUUID(),
        sessionId: session.id,
        role: "user",
        content,
        createdAt: Date.now(),
      },
    ]);
    setStreaming(true);
    await ipc.sendMessage(session.id, content);
  }

  return (
    <main className="container">
      <h1>FlowForge</h1>
      <div className="messages">
        {messages.map((m) => (
          <div key={m.id} className={`msg msg-${m.role}`}>
            <strong>{m.role}:</strong> {m.content}
          </div>
        ))}
      </div>
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <input
          value={input}
          onChange={(e) => setInput(e.currentTarget.value)}
          placeholder="Message FlowForge..."
        />
        <button type="submit" disabled={streaming}>
          {streaming ? "..." : "Send"}
        </button>
      </form>
    </main>
  );
}

export default App;
