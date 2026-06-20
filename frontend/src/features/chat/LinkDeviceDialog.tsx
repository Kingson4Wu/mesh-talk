import { useState } from "react";
import { Smartphone, KeyRound, Loader2 } from "lucide-react";
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
import { auth, chat } from "@/lib/api";
import { shortId } from "@/lib/format";
import { useChat } from "@/store/chat";

export function LinkDeviceDialog() {
  const myAccountId = useChat((s) => s.myAccountId);
  const peers = useChat((s) => s.peers);

  const [open, setOpen] = useState(false);
  const [code, setCode] = useState<string | null>(null);
  const [joinPeer, setJoinPeer] = useState("");
  const [joinCode, setJoinCode] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const onOpenChange = (v: boolean) => {
    setOpen(v);
    if (!v) {
      if (code) void chat.stopLinking();
      setCode(null);
      setMsg(null);
      setJoinCode("");
    }
  };

  const showCode = async () => {
    setMsg(null);
    try {
      setCode(await chat.startLinking());
    } catch (e) {
      setMsg(String(e));
    }
  };

  const doLink = async () => {
    if (!joinPeer || !joinCode.trim()) return;
    setBusy(true);
    setMsg(null);
    try {
      await chat.linkDevice(joinPeer, joinCode.trim());
      await auth.adoptLinkedAccount();
      setMsg("Linked! This device now shares that account.");
      setJoinCode("");
    } catch (e) {
      setMsg(`Link failed: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  const rekey = async () => {
    setBusy(true);
    setMsg(null);
    try {
      const id = await chat.rekeyAccount();
      setMsg(`Re-keyed. New account: ${shortId(id, 12)}…`);
    } catch (e) {
      setMsg(`Re-key failed: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" title="Link a device">
          <Smartphone className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Your devices</DialogTitle>
          <DialogDescription>
            This device's account:{" "}
            <code className="font-mono">
              {myAccountId ? `${shortId(myAccountId, 12)}…` : "—"}
            </code>
          </DialogDescription>
        </DialogHeader>

        <section className="space-y-2 rounded-lg border p-3">
          <p className="text-sm font-medium">Add another of your devices</p>
          <div className="flex items-center gap-2">
            <Button variant="secondary" size="sm" onClick={showCode}>
              Show pairing code
            </Button>
            {code && (
              <code className="rounded bg-muted px-2 py-1 font-mono text-sm tracking-widest">
                {code}
              </code>
            )}
          </div>
          <p className="text-xs text-muted-foreground">
            Enter this code on your other device, below.
          </p>
        </section>

        <section className="space-y-2 rounded-lg border p-3">
          <p className="text-sm font-medium">Have a code from your other device?</p>
          <select
            value={joinPeer}
            onChange={(e) => setJoinPeer(e.target.value)}
            className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
          >
            <option value="">Pick the device…</option>
            {peers.map((p) => (
              <option key={p.user_id} value={p.user_id}>
                {p.name || "(unnamed)"} ({shortId(p.user_id)})
              </option>
            ))}
          </select>
          <div className="flex gap-2">
            <Input
              placeholder="pairing code"
              value={joinCode}
              onChange={(e) => setJoinCode(e.target.value)}
            />
            <Button disabled={!joinPeer || !joinCode.trim() || busy} onClick={doLink}>
              {busy && <Loader2 className="h-4 w-4 animate-spin" />}
              Link
            </Button>
          </div>
        </section>

        <section className="flex items-center justify-between rounded-lg border border-destructive/30 p-3">
          <div>
            <p className="text-sm font-medium">Lost or compromised device?</p>
            <p className="text-xs text-muted-foreground">
              Rotate to a fresh account identity.
            </p>
          </div>
          <Button variant="destructive" size="sm" disabled={busy} onClick={rekey}>
            <KeyRound className="h-4 w-4" />
            Re-key
          </Button>
        </section>

        {msg && <p className="text-sm font-medium text-primary">{msg}</p>}
      </DialogContent>
    </Dialog>
  );
}
