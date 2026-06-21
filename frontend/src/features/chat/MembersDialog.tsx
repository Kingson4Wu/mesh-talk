import { useState } from "react";
import { Users, UserPlus, X } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Avatar } from "@/components/ui/avatar";
import { shortId } from "@/lib/format";
import { useChat } from "@/store/chat";

export function MembersDialog() {
  const members = useChat((s) => s.members);
  const peers = useChat((s) => s.peers);
  const addMember = useChat((s) => s.addMember);
  const removeMember = useChat((s) => s.removeMember);
  const [open, setOpen] = useState(false);

  const memberIds = new Set(members.map((m) => m.user_id));
  const addable = peers.filter((p) => !memberIds.has(p.user_id));

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" title="Members">
          <Users className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Channel members ({members.length})</DialogTitle>
        </DialogHeader>

        <div className="max-h-48 space-y-1 overflow-y-auto">
          {members.map((m) => (
            <div
              key={m.user_id}
              className="flex items-center gap-3 rounded-md px-1 py-1.5"
            >
              <Avatar name={m.name} id={m.user_id} className="h-7 w-7" />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm">{m.name}</div>
                <div className="truncate font-mono text-xs text-muted-foreground">
                  {shortId(m.user_id, 12)}
                </div>
              </div>
              <Button
                variant="ghost"
                size="icon"
                className="h-7 w-7 text-muted-foreground hover:text-destructive"
                title="Remove"
                onClick={() => removeMember(m.user_id)}
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
          ))}
        </div>

        {addable.length > 0 && (
          <>
            <div className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
              Add a peer
            </div>
            <div className="max-h-40 space-y-1 overflow-y-auto rounded-lg border p-1">
              {addable.map((p) => (
                <button
                  key={p.user_id}
                  onClick={() => addMember(p.user_id)}
                  className="flex w-full items-center gap-3 rounded-md px-2 py-1.5 text-left hover:bg-accent/50"
                >
                  <Avatar name={p.name} id={p.user_id} className="h-7 w-7" />
                  <span className="flex-1 truncate text-sm">
                    {p.name || "(unnamed)"}
                  </span>
                  <UserPlus className="h-4 w-4 text-muted-foreground" />
                </button>
              ))}
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
