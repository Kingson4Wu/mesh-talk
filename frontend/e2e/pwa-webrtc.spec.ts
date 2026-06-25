import { test, expect } from "@playwright/test";

// Browser WebRTC client (mobile-PWA plan, Phase 2). Two real RTCPeerConnections connect through
// an in-page signaling pair (a stand-in for the relay — same {kind,sdp} wire format the Rust
// gateway uses) and round-trip a message both ways. Proves the offer/answer handshake + data
// channel work in a real browser; the relay itself + the Rust answerer are tested on the Rust
// side, and they share this exact wire format.

const CLIENT = "/src/lib/" + "webrtcClient.ts";

test("browser WebRTC data channel round-trips via signaling", async ({
  page,
}) => {
  await page.goto("/");
  const result = await page.evaluate(async (mod) => {
    const { connect } = await import(/* @vite-ignore */ mod);

    // A pair of signaling endpoints that forward to each other (stands in for the room relay).
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

    const [offChan, ansChan] = await Promise.all([
      connect(a, "offerer"),
      connect(b, "answerer"),
    ]);

    const recv = (ch: RTCDataChannel) =>
      new Promise<string>((resolve) => {
        ch.onmessage = (e) => resolve(e.data as string);
      });

    const ansGot = recv(ansChan);
    offChan.send("ping");
    const a2b = await ansGot;

    const offGot = recv(offChan);
    ansChan.send("pong");
    const b2a = await offGot;

    return { a2b, b2a };
  }, CLIENT);

  expect(result.a2b).toBe("ping");
  expect(result.b2a).toBe("pong");
});
