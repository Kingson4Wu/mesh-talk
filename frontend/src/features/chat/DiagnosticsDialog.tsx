import { useCallback, useEffect, useRef, useState } from "react";
import { Activity, Copy, Radar, Trash2, Wifi } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { diag } from "@/lib/api";
import { formatAgo, formatTime, shortId } from "@/lib/format";
import type { DiagNetworkInfo, DiagPeerInfo } from "@/lib/types";

const POLL_MS = 1500;
const LOG_CAP = 200;

interface LogLine {
  ts: number;
  text: string;
}

/** A monospace value that copies itself to the clipboard when clicked. */
function CopyValue({ value, label }: { value: string; label?: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(value).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1000);
      },
      () => {},
    );
  };
  return (
    <button
      type="button"
      onClick={copy}
      title={t("common.copy")}
      className="group inline-flex max-w-full items-center gap-1 font-mono text-xs hover:text-foreground"
    >
      <span className="truncate">{label ?? value}</span>
      <Copy className="h-3 w-3 shrink-0 opacity-40 group-hover:opacity-100" />
      {copied && (
        <span className="text-[10px] text-primary">{t("common.copied")}</span>
      )}
    </button>
  );
}

function diffPeers(
  prev: DiagPeerInfo[],
  next: DiagPeerInfo[],
  t: TFunction,
): LogLine[] {
  const ts = Date.now();
  const prevIds = new Set(prev.map((p) => p.user_id));
  const nextIds = new Set(next.map((p) => p.user_id));
  const lines: LogLine[] = [];
  for (const p of next) {
    if (!prevIds.has(p.user_id)) {
      lines.push({
        ts,
        text: t("diagnostics.peerAppeared", {
          name: p.name || t("common.unnamed"),
          ip: p.ip,
        }),
      });
    }
  }
  for (const p of prev) {
    if (!nextIds.has(p.user_id)) {
      lines.push({
        ts,
        text: t("diagnostics.peerDisappeared", {
          name: p.name || t("common.unnamed"),
        }),
      });
    }
  }
  return lines;
}

export function DiagnosticsDialog() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [info, setInfo] = useState<DiagNetworkInfo | null>(null);
  const [peers, setPeers] = useState<DiagPeerInfo[]>([]);
  const [log, setLog] = useState<LogLine[]>([]);
  const prevPeers = useRef<DiagPeerInfo[]>([]);

  const poll = useCallback(async () => {
    try {
      const next = await diag.getPeers();
      const lines = diffPeers(prevPeers.current, next, t);
      prevPeers.current = next;
      setPeers(next);
      if (lines.length > 0) {
        setLog((l) => [...lines.reverse(), ...l].slice(0, LOG_CAP));
      }
    } catch {
      /* node may still be starting; ignore */
    }
  }, [t]);

  useEffect(() => {
    if (!open) return;
    // Reset the diff baseline each time the panel opens so we don't replay a stale snapshot.
    prevPeers.current = [];
    void diag.networkInfo().then(setInfo, () => setInfo(null));
    void poll();
    const id = setInterval(() => void poll(), POLL_MS);
    return () => clearInterval(id);
  }, [open, poll]);

  // Group discovered peers by account (devices sharing an account_id are one user).
  const grouped = new Map<string, DiagPeerInfo[]>();
  const solo: DiagPeerInfo[] = [];
  for (const p of peers) {
    if (p.account_id) {
      const arr = grouped.get(p.account_id) ?? [];
      arr.push(p);
      grouped.set(p.account_id, arr);
    } else {
      solo.push(p);
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" title={t("diagnostics.trigger")}>
          <Radar className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Radar className="h-4 w-4" /> {t("diagnostics.title")}
          </DialogTitle>
          <DialogDescription>{t("diagnostics.description")}</DialogDescription>
        </DialogHeader>

        <div className="grid max-h-[70vh] gap-4 overflow-y-auto">
          {/* This device */}
          <section className="space-y-2 rounded-lg border p-3">
            <p className="flex items-center gap-2 text-sm font-semibold">
              <Wifi className="h-4 w-4" /> {t("diagnostics.thisDevice")}
            </p>
            {info ? (
              <dl className="grid grid-cols-[auto,1fr] gap-x-4 gap-y-1 text-sm">
                <dt className="text-muted-foreground">
                  {t("diagnostics.name")}
                </dt>
                <dd className="truncate">{info.own_name || "—"}</dd>
                <dt className="text-muted-foreground">
                  {t("diagnostics.fingerprint")}
                </dt>
                <dd>
                  <CopyValue value={info.own_user_id} />
                </dd>
                <dt className="text-muted-foreground">
                  {t("diagnostics.account")}
                </dt>
                <dd>
                  <CopyValue value={info.account_id} />
                </dd>
                <dt className="text-muted-foreground">
                  {t("diagnostics.listenPort")}
                </dt>
                <dd className="font-mono text-xs">{info.listen_tcp_port}</dd>
                <dt className="text-muted-foreground">
                  {t("diagnostics.discoveryPort")}
                </dt>
                <dd className="font-mono text-xs">{info.discovery_port}</dd>
                <dt className="text-muted-foreground">
                  {t("diagnostics.multicastGroup")}
                </dt>
                <dd className="font-mono text-xs">{info.multicast_group}</dd>
                <dt className="text-muted-foreground">
                  {t("diagnostics.interfaces")}
                </dt>
                <dd className="flex flex-wrap gap-1">
                  {info.interfaces.length === 0 ? (
                    <span className="text-xs text-muted-foreground">
                      {t("diagnostics.noneFound")}
                    </span>
                  ) : (
                    info.interfaces.map((ip) => (
                      <CopyValue key={ip} value={ip} />
                    ))
                  )}
                </dd>
              </dl>
            ) : (
              <p className="text-sm text-muted-foreground">
                {t("diagnostics.nodeNotReady")}
              </p>
            )}
          </section>

          {/* Discovered peers */}
          <section className="space-y-2 rounded-lg border p-3">
            <p className="flex items-center gap-2 text-sm font-semibold">
              <Radar className="h-4 w-4" /> {t("diagnostics.discoveredPeers")}
              <Badge className="bg-muted text-muted-foreground">
                {peers.length}
              </Badge>
            </p>
            {peers.length === 0 && (
              <p className="text-sm text-muted-foreground">
                {t("diagnostics.noPeers")}
              </p>
            )}
            <div className="space-y-2">
              {[...grouped.entries()].map(([acct, devices]) => (
                <div key={acct} className="rounded-md bg-muted/40 p-2">
                  <div className="mb-1 flex items-center gap-2 text-xs text-muted-foreground">
                    <span>{t("diagnostics.account")}</span>
                    <CopyValue value={acct} label={`${shortId(acct, 12)}…`} />
                    <span>
                      ·{" "}
                      {t("diagnostics.deviceCount", { count: devices.length })}
                    </span>
                  </div>
                  {devices.map((p) => (
                    <PeerRow key={p.user_id} p={p} />
                  ))}
                </div>
              ))}
              {solo.map((p) => (
                <div key={p.user_id} className="rounded-md bg-muted/40 p-2">
                  <PeerRow p={p} />
                </div>
              ))}
            </div>
          </section>

          {/* Activity log */}
          <section className="space-y-2 rounded-lg border p-3">
            <div className="flex items-center justify-between">
              <p className="flex items-center gap-2 text-sm font-semibold">
                <Activity className="h-4 w-4" /> {t("diagnostics.activityLog")}
              </p>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setLog([])}
                disabled={log.length === 0}
              >
                <Trash2 className="h-3.5 w-3.5" /> {t("common.clear")}
              </Button>
            </div>
            {log.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {t("diagnostics.noActivity")}
              </p>
            ) : (
              <ul className="max-h-40 space-y-0.5 overflow-y-auto text-xs">
                {log.map((l, i) => (
                  <li key={i} className="flex gap-2">
                    <span className="shrink-0 font-mono text-muted-foreground">
                      {formatTime(l.ts)}
                    </span>
                    <span>{l.text}</span>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function PeerRow({ p }: { p: DiagPeerInfo }) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center justify-between gap-2 py-1">
      <div className="min-w-0">
        <div className="flex items-center gap-2">
          <span className="truncate text-sm font-medium">
            {p.name || t("common.unnamed")}
          </span>
          {p.post_office && (
            <Badge className="bg-primary/15 text-primary">
              {t("diagnostics.postOffice")}
            </Badge>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-x-3 text-xs text-muted-foreground">
          <CopyValue value={p.user_id} label={shortId(p.user_id, 12)} />
          <CopyValue value={`${p.ip}:${p.tcp_port}`} />
        </div>
      </div>
      <span className="shrink-0 text-xs text-muted-foreground">
        {formatAgo(p.last_seen_secs)}
      </span>
    </div>
  );
}
