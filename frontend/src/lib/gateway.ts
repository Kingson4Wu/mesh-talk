// Browser gateway connect-and-sync (mobile-PWA plan, Phase 2). Combines the two PWA-side
// pieces: the WebRTC client opens a data channel to the node (via the signaling relay), then the
// wasm core runs the mesh's Noise SecureChannel + event-log sync over it. This is the glue the
// app's backend calls to message over the gateway. See
// docs/superpowers/specs/2026-06-23-mobile-pwa-design.md.

import { connect, wsSignaling, type Signaling } from "./webrtcClient";
import { loadWasm } from "./wasm";

/**
 * Establish a data channel to the peer in `signaling` (offerer if `initiator`), then run the
 * mesh event-log sync over it via the wasm core. Returns the local event count afterwards.
 * Phase 2 demo entry point; the per-conversation send/receive API grows on top of this.
 */
export async function syncOverGateway(
  signaling: Signaling,
  initiator: boolean,
  seed: boolean,
): Promise<number> {
  const dc = await connect(signaling, initiator ? "offerer" : "answerer");
  const m = await loadWasm();
  return m.sync_demo_over_dc(dc, initiator, seed);
}

/** Production entry: connect to `room` on the signaling relay at `url`, then sync. */
export async function connectAndSync(
  url: string,
  room: string,
  initiator: boolean,
  seed: boolean,
): Promise<number> {
  const signaling = await wsSignaling(url, room);
  return syncOverGateway(signaling, initiator, seed);
}
