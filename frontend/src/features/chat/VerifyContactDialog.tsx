import { useEffect, useState } from "react";
import {
  ShieldCheck,
  ShieldAlert,
  ShieldQuestion,
  Loader2,
} from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { chat } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { useChat } from "@/store/chat";
import type { SafetyNumber, TrustInfo } from "@/lib/types";

/**
 * Contact verification / safety-number UI. A contact is identified by its account id;
 * its current device fingerprint comes from the discovery roster. We show the
 * fingerprint as a readable safety number (grouped digits + a word sequence) that the
 * two users compare out-of-band, a "Mark as verified" action, a verified badge, and a
 * loud warning if a known contact's fingerprint changed since first contact.
 *
 * This is pure trust UX over the existing fingerprints — no crypto changes.
 */
export function VerifyContactDialog({
  accountId,
  name,
}: {
  accountId: string;
  name: string;
}) {
  const peers = useChat((s) => s.peers);
  // The current device fingerprint presenting this account (any of its devices).
  const fingerprint =
    peers.find((p) => p.account_id === accountId)?.user_id ?? "";

  const [open, setOpen] = useState(false);
  const [trust, setTrust] = useState<TrustInfo | null>(null);
  const [sn, setSn] = useState<SafetyNumber | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (!open || !fingerprint) return;
    let active = true;
    setErr(null);
    void (async () => {
      try {
        const [t, s] = await Promise.all([
          chat.getTrust(accountId, fingerprint),
          chat.safetyNumber(fingerprint),
        ]);
        if (active) {
          setTrust(t);
          setSn(s);
        }
      } catch (e) {
        if (active) setErr(errorMessage(e));
      }
    })();
    return () => {
      active = false;
    };
  }, [open, accountId, fingerprint]);

  const verify = async () => {
    if (!fingerprint) return;
    setBusy(true);
    setErr(null);
    try {
      await chat.markVerified(accountId, fingerprint);
      setTrust(await chat.getTrust(accountId, fingerprint));
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const verified = trust?.verified ?? false;
  const changed = trust?.fingerprint_changed ?? false;

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          title="Verify safety number"
          aria-label="Verify safety number"
        >
          {changed ? (
            <ShieldAlert className="h-4 w-4 text-destructive" />
          ) : verified ? (
            <ShieldCheck className="h-4 w-4 text-emerald-500" />
          ) : (
            <ShieldQuestion className="h-4 w-4" />
          )}
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            Verify {name}
            {verified && !changed && (
              <Badge className="bg-emerald-500/15 text-emerald-600 dark:text-emerald-400">
                <ShieldCheck className="mr-1 h-3 w-3" /> Verified
              </Badge>
            )}
          </DialogTitle>
          <DialogDescription>
            Compare this safety number with {name} over a trusted channel (in
            person, a phone call). If it matches, there is no one in the middle.
          </DialogDescription>
        </DialogHeader>

        {changed && (
          <div className="flex items-start gap-2 rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
            <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
            <div>
              <p className="font-semibold">
                This contact's safety number changed.
              </p>
              <p className="text-xs">
                Their device fingerprint differs from what you first saw. This
                happens when they add or replace a device — but it can also be
                an impostor. Re-verify before trusting it.
              </p>
            </div>
          </div>
        )}

        {!fingerprint && (
          <p className="text-sm text-muted-foreground">
            This contact isn't currently online, so its fingerprint can't be
            shown.
          </p>
        )}

        {sn && fingerprint && (
          <div className="space-y-3 rounded-lg border p-3">
            <div>
              <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Safety number
              </p>
              <code className="mt-1 block break-all font-mono text-sm leading-relaxed">
                {sn.grouped}
              </code>
            </div>
            {sn.words.length > 0 && (
              <div className="flex flex-wrap gap-1.5">
                {sn.words.map((w, i) => (
                  <Badge
                    key={`${w}-${i}`}
                    className="bg-muted capitalize text-muted-foreground"
                  >
                    {w}
                  </Badge>
                ))}
              </div>
            )}
          </div>
        )}

        {err && <p className="text-sm text-destructive">{err}</p>}

        <div className="flex justify-end">
          <Button disabled={!fingerprint || busy} onClick={verify}>
            {busy && <Loader2 className="h-4 w-4 animate-spin" />}
            {verified && !changed ? "Re-verify" : "Mark as verified"}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
