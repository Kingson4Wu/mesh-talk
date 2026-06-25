// Browser WebRTC client for the gateway (mobile-PWA plan, Phase 2). The JS counterpart to the
// node's Rust gateway: it opens a data channel to the other party in a signaling room, then the
// node relays the (Noise-encrypted) frames into the mesh. Signaling is NON-trickle — gather all
// ICE candidates into the SDP, then send one offer / one answer — matching the Rust side's wire
// format exactly. See docs/superpowers/specs/2026-06-23-mobile-pwa-design.md.

/** The signaling payloads, identical to the Rust `Signal` enum: {kind:"offer"|"answer", sdp}. */
export type SignalMsg = { kind: "offer" | "answer"; sdp: string };

/** A bidirectional signaling channel between the two peers in a room. */
export interface Signaling {
  send(msg: SignalMsg): void;
  onMessage(cb: (msg: SignalMsg) => void): void;
  close(): void;
}

/** Resolve once this peer connection has finished gathering ICE candidates (non-trickle). */
function gatheringComplete(pc: RTCPeerConnection): Promise<void> {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const check = () => {
      if (pc.iceGatheringState === "complete") {
        pc.removeEventListener("icegatheringstatechange", check);
        resolve();
      }
    };
    pc.addEventListener("icegatheringstatechange", check);
  });
}

/**
 * Establish a data channel to the peer over `signaling`. The offerer creates the channel + offer
 * (the PWA's role); the answerer waits + answers (the node). Resolves with the open channel.
 */
export function connect(
  signaling: Signaling,
  role: "offerer" | "answerer",
): Promise<RTCDataChannel> {
  const pc = new RTCPeerConnection();

  const opened = new Promise<RTCDataChannel>((resolve, reject) => {
    pc.addEventListener("connectionstatechange", () => {
      if (pc.connectionState === "failed")
        reject(new Error("WebRTC connection failed"));
    });
    if (role === "offerer") {
      const dc = pc.createDataChannel("mesh");
      dc.onopen = () => resolve(dc);
    } else {
      pc.ondatachannel = (e) => {
        e.channel.onopen = () => resolve(e.channel);
      };
    }
  });

  signaling.onMessage(async (msg) => {
    if (role === "offerer" && msg.kind === "answer") {
      await pc.setRemoteDescription({ type: "answer", sdp: msg.sdp });
    } else if (role === "answerer" && msg.kind === "offer") {
      await pc.setRemoteDescription({ type: "offer", sdp: msg.sdp });
      const answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);
      await gatheringComplete(pc);
      signaling.send({ kind: "answer", sdp: pc.localDescription!.sdp });
    }
  });

  if (role === "offerer") {
    void (async () => {
      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      await gatheringComplete(pc);
      signaling.send({ kind: "offer", sdp: pc.localDescription!.sdp });
    })();
  }

  return opened;
}

/**
 * Like `wsSignaling`, but asks the relay to assign this peer's WebRTC role (so two peers don't
 * both offer): resolves with the signaling channel + whether we are the initiator (offerer). The
 * first peer in a room becomes the answerer, the next the offerer.
 */
export function wsSignalingWithRole(
  url: string,
  room: string,
): Promise<{ signaling: Signaling; initiator: boolean }> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    let handler: ((m: SignalMsg) => void) | null = null;
    let resolved = false;
    ws.onmessage = (e) => {
      try {
        const m = JSON.parse(e.data as string);
        if (m && m.kind === "role" && !resolved) {
          resolved = true;
          resolve({
            signaling: {
              send: (msg) => ws.send(JSON.stringify(msg)),
              onMessage: (cb) => {
                handler = cb;
              },
              close: () => ws.close(),
            },
            initiator: !!m.initiator,
          });
        } else if (m && (m.kind === "offer" || m.kind === "answer")) {
          handler?.(m);
        }
      } catch {
        /* ignore non-JSON frames */
      }
    };
    ws.onerror = () => reject(new Error("signaling websocket error"));
    ws.onopen = () =>
      ws.send(JSON.stringify({ type: "join", room, want_role: true }));
  });
}

/**
 * Multi-peer signaling channel (relay mesh mode): the relay assigns us a numeric id, tells us the
 * existing mesh peers, announces peers joining/leaving, and routes frames addressed by peer id.
 * This is what lets ONE hub hold a separate connection to each of MANY spokes in one room — no
 * 2-peer limit. Pair it with `meshPeerSignaling` + `connect` for each hub↔spoke connection.
 */
export interface MeshChannel {
  /** Our own relay-assigned id. */
  myId: number;
  /** Mesh peers already in the room when we joined (oldest first; `[0]` is the hub). */
  peers: number[];
  /** Subscribe to peers joining/leaving. */
  onPeer(cb: (ev: { type: "joined" | "left"; id: number }) => void): void;
  /** Send a signal addressed to one peer. */
  sendTo(to: number, msg: SignalMsg): void;
  /** Register the handler for signals from one peer (replaces any prior one). */
  onFrom(peer: number, cb: (msg: SignalMsg) => void): void;
  /** Fires when the signaling socket closes (relay gone) — the cue to reconnect / fail over. */
  onClose(cb: () => void): void;
  close(): void;
}

export function wsSignalingMesh(
  url: string,
  room: string,
): Promise<MeshChannel> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const fromHandlers = new Map<number, (m: SignalMsg) => void>();
    let peerCb: ((ev: { type: "joined" | "left"; id: number }) => void) | null =
      null;
    let closeCb: (() => void) | null = null;
    let settled = false;
    ws.onclose = () => closeCb?.();
    ws.onmessage = (e) => {
      let m: {
        kind?: string;
        you?: number;
        peers?: number[];
        peer?: number;
        from?: number;
        sdp?: string;
      };
      try {
        m = JSON.parse(e.data as string);
      } catch {
        return;
      }
      if (!m || !m.kind) return;
      if (m.kind === "welcome" && !settled) {
        settled = true;
        resolve({
          myId: m.you ?? 0,
          peers: m.peers ?? [],
          onPeer: (cb) => {
            peerCb = cb;
          },
          sendTo: (to, msg) => ws.send(JSON.stringify({ ...msg, to })),
          onFrom: (peer, cb) => {
            fromHandlers.set(peer, cb);
          },
          onClose: (cb) => {
            closeCb = cb;
          },
          close: () => ws.close(),
        });
      } else if (m.kind === "peer-joined" && typeof m.peer === "number") {
        peerCb?.({ type: "joined", id: m.peer });
      } else if (m.kind === "peer-left" && typeof m.peer === "number") {
        peerCb?.({ type: "left", id: m.peer });
      } else if (
        (m.kind === "offer" || m.kind === "answer") &&
        typeof m.from === "number"
      ) {
        fromHandlers.get(m.from)?.({ kind: m.kind, sdp: m.sdp ?? "" });
      }
    };
    ws.onerror = () => reject(new Error("signaling websocket error"));
    ws.onopen = () =>
      ws.send(JSON.stringify({ type: "join", room, mesh: true }));
  });
}

/** Adapt a mesh channel + one peer id into the 1:1 `Signaling` that `connect` expects. */
export function meshPeerSignaling(mesh: MeshChannel, peer: number): Signaling {
  return {
    send: (msg) => mesh.sendTo(peer, msg),
    onMessage: (cb) => mesh.onFrom(peer, cb),
    close: () => {},
  };
}

/** Production signaling: a WebSocket to the relay (crates/mesh-talk-signal), joined to `room`. */
export function wsSignaling(url: string, room: string): Promise<Signaling> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    let handler: ((m: SignalMsg) => void) | null = null;
    ws.onmessage = (e) => {
      try {
        const m = JSON.parse(e.data as string);
        if (m && (m.kind === "offer" || m.kind === "answer")) handler?.(m);
      } catch {
        /* ignore non-JSON frames */
      }
    };
    ws.onerror = () => reject(new Error("signaling websocket error"));
    ws.onopen = () => {
      ws.send(JSON.stringify({ type: "join", room }));
      resolve({
        send: (msg) => ws.send(JSON.stringify(msg)),
        onMessage: (cb) => {
          handler = cb;
        },
        close: () => ws.close(),
      });
    };
  });
}
