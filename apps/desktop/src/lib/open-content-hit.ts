// Open a content-search hit (#710, shared by both #876 search surfaces):
// switch to its session (pane-aware — loads into the focused pane so it
// appears where the user is looking, falling back to a global switch), then
// open the find bar seeded with the query + the hit's messageId so the
// thread scrolls to and highlights the matched message.

import { usePanesStore } from "@/store/panes";
import { useChatStore } from "@/store/chat";
import { useFindStore } from "@/store/find";
import type { SearchHit } from "@/bindings/SearchHit";

export function openContentHit(hit: SearchHit, query: string): void {
  const panes = usePanesStore.getState();
  const chat = useChatStore.getState();
  if (panes.focusedPaneId) {
    panes.setPaneSession(panes.focusedPaneId, hit.sessionId);
    void chat.loadSession(hit.sessionId);
  } else {
    void chat.selectSession(hit.sessionId);
  }
  useFindStore.getState().openFind(hit.sessionId, {
    query,
    messageId: hit.messageId,
  });
}
