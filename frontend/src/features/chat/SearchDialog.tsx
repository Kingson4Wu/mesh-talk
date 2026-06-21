import { useEffect, useRef, useState } from "react";
import { Search } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { chat } from "@/lib/api";
import { formatDay } from "@/lib/format";
import { useChat, type Conversation } from "@/store/chat";
import type { SearchHitInfo } from "@/lib/types";

export function SearchDialog() {
  const open = useChat((s) => s.open);
  const peers = useChat((s) => s.peers);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHitInfo[]>([]);
  const [searching, setSearching] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>();

  useEffect(() => {
    if (!dialogOpen) return;
    if (!query.trim()) {
      setHits([]);
      return;
    }
    setSearching(true);
    clearTimeout(timer.current);
    // `active` guards against a stale resolution: if the query changes (or the dialog
    // closes) while a search is in flight, its result must not overwrite the newer one.
    let active = true;
    timer.current = setTimeout(async () => {
      try {
        const results = await chat.search(query.trim());
        if (active) setHits(results);
      } catch {
        if (active) setHits([]);
      } finally {
        if (active) setSearching(false);
      }
    }, 250);
    return () => {
      active = false;
      clearTimeout(timer.current);
    };
  }, [query, dialogOpen]);

  const go = (h: SearchHitInfo) => {
    let conv: Conversation;
    if (h.is_channel) {
      conv = { kind: "channel", id: h.target, name: h.label };
    } else {
      // Resolve a device-id target to its account so account_history works.
      const peer = peers.find((p) => p.user_id === h.target);
      conv = {
        kind: "account",
        id: peer?.account_id ?? h.target,
        name: h.label,
      };
    }
    void open(conv);
    setDialogOpen(false);
  };

  return (
    <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" title="Search messages">
          <Search className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Search messages</DialogTitle>
        </DialogHeader>
        <Input
          autoFocus
          placeholder="Search across all conversations…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="max-h-80 space-y-1 overflow-y-auto">
          {searching && (
            <p className="py-4 text-center text-sm text-muted-foreground">
              Searching…
            </p>
          )}
          {!searching && query.trim() && hits.length === 0 && (
            <p className="py-4 text-center text-sm text-muted-foreground">
              No matches.
            </p>
          )}
          {hits.map((h, i) => (
            <button
              key={i}
              onClick={() => go(h)}
              className="block w-full rounded-lg px-3 py-2 text-left hover:bg-accent"
            >
              <div className="flex items-center justify-between gap-2">
                <span className="truncate text-sm font-medium">
                  {h.is_channel ? "#" : ""}
                  {h.label}
                </span>
                <span className="shrink-0 text-xs text-muted-foreground">
                  {formatDay(h.wall_clock)}
                </span>
              </div>
              <div className="truncate text-sm text-muted-foreground">
                <span className="text-foreground/70">
                  {h.from_me ? "you" : h.who}:
                </span>{" "}
                {h.text}
              </div>
            </button>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
