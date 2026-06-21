import { useState } from "react";
import { Plus, Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Avatar } from "@/components/ui/avatar";
import { cn } from "@/lib/utils";
import { shortId } from "@/lib/format";
import { useChat } from "@/store/chat";

export function CreateChannelDialog() {
  const { t } = useTranslation();
  const peers = useChat((s) => s.peers);
  const createChannel = useChat((s) => s.createChannel);

  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [busy, setBusy] = useState(false);

  const toggle = (id: string) => setSelected((s) => ({ ...s, [id]: !s[id] }));

  const submit = async () => {
    const ids = Object.keys(selected).filter((k) => selected[k]);
    if (!name.trim()) return;
    setBusy(true);
    try {
      await createChannel(name.trim(), ids);
      setOpen(false);
      setName("");
      setSelected({});
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="h-6 w-6"
          title={t("createChannel.trigger")}
        >
          <Plus className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("createChannel.title")}</DialogTitle>
          <DialogDescription>
            {t("createChannel.description")}
          </DialogDescription>
        </DialogHeader>

        <Input
          autoFocus
          placeholder={t("createChannel.namePlaceholder")}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />

        <div className="max-h-56 space-y-1 overflow-y-auto rounded-lg border p-1">
          {peers.length === 0 && (
            <p className="px-2 py-3 text-center text-sm text-muted-foreground">
              {t("createChannel.noPeers")}
            </p>
          )}
          {peers.map((p) => (
            <button
              key={p.user_id}
              onClick={() => toggle(p.user_id)}
              className="flex w-full items-center gap-3 rounded-md px-2 py-1.5 text-left hover:bg-accent/50"
            >
              <Avatar name={p.name} id={p.user_id} className="h-7 w-7" />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm">
                  {p.name || t("common.unnamed")}
                </div>
                <div className="truncate font-mono text-xs text-muted-foreground">
                  {shortId(p.user_id, 12)}
                </div>
              </div>
              <span
                className={cn(
                  "flex h-4 w-4 items-center justify-center rounded border",
                  selected[p.user_id]
                    ? "border-primary bg-primary text-primary-foreground"
                    : "border-input",
                )}
              >
                {selected[p.user_id] && "✓"}
              </span>
            </button>
          ))}
        </div>

        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setOpen(false)}>
            {t("common.cancel")}
          </Button>
          <Button disabled={!name.trim() || busy} onClick={submit}>
            {busy && <Loader2 className="h-4 w-4 animate-spin" />}
            {t("common.create")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
