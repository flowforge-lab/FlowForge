import { useEffect, useState } from "react";
import { Download, Search } from "@/components/ui/icon";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { ipc } from "@/lib/ipc";
import type { MarketplaceProfile } from "@/lib/profile-marketplace";

type LoadState = "loading" | "ready" | "error";

const numberFmt = new Intl.NumberFormat();

/**
 * Marketplace sub-tab (SET.7): a search over the (mock) profile catalog with result
 * cards and a skeleton while loading. Empty query lists the full catalog. Install
 * isn't wired — the card's Install button surfaces that it arrives with the backend.
 */
export function MarketplaceTab() {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<MarketplaceProfile[]>([]);
  const [state, setState] = useState<LoadState>("loading");

  useEffect(() => {
    let alive = true;
    // Debounce so each keystroke doesn't fire a search. setState runs only inside
    // callbacks (not synchronously in the effect body); previous results stay
    // visible during the debounce window, avoiding a flash.
    const handle = setTimeout(() => {
      if (!alive) return;
      setState("loading");
      ipc
        .searchProfileMarketplace(query)
        .then((r) => {
          if (!alive) return;
          setResults(r);
          setState("ready");
        })
        .catch(() => alive && setState("error"));
    }, 200);
    return () => {
      alive = false;
      clearTimeout(handle);
    };
  }, [query]);

  return (
    <div className="space-y-4">
      <div className="relative">
        <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={query}
          placeholder="Search the profile marketplace…"
          autoComplete="off"
          className="pl-8"
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      {state === "loading" ? (
        <MarketplaceSkeleton />
      ) : state === "error" ? (
        <p className="text-[12px] text-destructive" role="alert">
          Couldn’t reach the marketplace.
        </p>
      ) : results.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">
          No profiles match “{query.trim()}”.
        </p>
      ) : (
        <ul className="flex flex-col gap-2">
          {results.map((profile) => (
            <li
              key={profile.id}
              className="flex items-start gap-3 rounded-md border px-3 py-2.5"
            >
              <div className="min-w-0 flex-1 space-y-1">
                <span className="block truncate text-[13px] font-medium text-foreground">
                  {profile.name}
                </span>
                <p className="text-[11px] leading-relaxed text-muted-foreground">
                  {profile.description}
                </p>
                <p className="text-[10px] text-muted-foreground">
                  {profile.author} · {profile.skillCount} skills ·{" "}
                  {numberFmt.format(profile.installs)} installs
                </p>
              </div>
              <Button
                type="button"
                variant="outline"
                size="xs"
                className="mt-0.5 shrink-0"
                disabled
                title="Installing from the marketplace arrives with the backend"
              >
                <Download />
                Install
              </Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Three pulsing placeholder rows shown while a search is in flight. */
function MarketplaceSkeleton() {
  return (
    <ul className="flex flex-col gap-2" aria-hidden>
      {[0, 1, 2].map((i) => (
        <li key={i} className="rounded-md border px-3 py-2.5">
          <div className="space-y-2">
            <div className="h-3 w-1/3 animate-pulse rounded bg-muted" />
            <div className="h-2.5 w-3/4 animate-pulse rounded bg-muted" />
            <div className="h-2.5 w-1/4 animate-pulse rounded bg-muted" />
          </div>
        </li>
      ))}
    </ul>
  );
}
