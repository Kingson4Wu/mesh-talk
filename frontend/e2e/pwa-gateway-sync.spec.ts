import { test, expect } from "@playwright/test";

// The integrated PWA-side gateway flow (mobile-PWA plan, Phase 2): the WebRTC client opens a
// data channel through a signaling channel, then the wasm core runs the mesh event-log sync over
// it — i.e. lib/gateway.ts's syncOverGateway end to end. Two PWAs connect via an in-page
// signaling pair (the relay is exercised separately, Rust-side) and converge on a seeded event.
test.setTimeout(30_000);

const GATEWAY = "/src/lib/" + "gateway.ts";

test("syncOverGateway: two PWAs connect + sync a mesh event", async ({
  page,
}) => {
  await page.goto("/");
  const result = await page.evaluate(async (mod) => {
    const { syncOverGateway } = await import(/* @vite-ignore */ mod);

    // In-page signaling pair forwarding to each other (stands in for the relay room).
    type Msg = { kind: "offer" | "answer"; sdp: string };
    let aCb: ((m: Msg) => void) | null = null;
    let bCb: ((m: Msg) => void) | null = null;
    const a = {
      send: (m: Msg) => bCb && bCb(m),
      onMessage: (cb: (m: Msg) => void) => (aCb = cb),
      close() {},
    };
    const b = {
      send: (m: Msg) => aCb && aCb(m),
      onMessage: (cb: (m: Msg) => void) => (bCb = cb),
      close() {},
    };

    // A (offerer) seeded with one event; B (answerer) empty. Both should converge to it.
    const [countA, countB] = await Promise.all([
      syncOverGateway(a, true, true),
      syncOverGateway(b, false, false),
    ]);
    return { countA, countB };
  }, GATEWAY);

  expect(result.countA).toBe(1);
  expect(result.countB).toBe(1);
});
