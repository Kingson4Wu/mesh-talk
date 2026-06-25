import { test, expect } from "@playwright/test";

// The wasm mesh SecureChannel (Noise XX) running over REAL browser RTCDataChannels (mobile-PWA
// plan, Phase 2). Two RTCPeerConnections connect in-page (loopback), then each side runs the
// wasm handshake over its data channel — the initiator and responder authenticate each other.
// Proves the DataChannelStream bridge + the wasm Noise channel work over actual WebRTC, the
// browser counterpart to the node gateway. PBKDF2-free, but allow room for WebRTC setup.
test.setTimeout(30_000);

const WASM = "/src/wasm/" + "mesh_talk_wasm.js";

test("wasm Noise SecureChannel runs over a real RTCDataChannel", async ({
  page,
}) => {
  await page.goto("/");
  const result = await page.evaluate(async (wasmPath) => {
    const mod = await import(/* @vite-ignore */ wasmPath);
    await mod.default();

    const a = new RTCPeerConnection();
    const b = new RTCPeerConnection();
    a.onicecandidate = (e) => e.candidate && b.addIceCandidate(e.candidate);
    b.onicecandidate = (e) => e.candidate && a.addIceCandidate(e.candidate);

    const dcA = a.createDataChannel("mesh");
    const dcBReady = new Promise<RTCDataChannel>((res) => {
      b.ondatachannel = (e) => res(e.channel);
    });
    const openA = new Promise<void>((res) => {
      dcA.onopen = () => res();
    });

    const offer = await a.createOffer();
    await a.setLocalDescription(offer);
    await b.setRemoteDescription(offer);
    const answer = await b.createAnswer();
    await b.setLocalDescription(answer);
    await a.setRemoteDescription(answer);

    const dcB = await dcBReady;
    const openB = new Promise<void>((res) => {
      if (dcB.readyState === "open") res();
      else dcB.onopen = () => res();
    });
    await Promise.all([openA, openB]);

    // Each side runs the wasm Noise handshake over its data channel, concurrently.
    const [fpA, fpB] = await Promise.all([
      mod.secure_handshake_over_dc(dcA, true),
      mod.secure_handshake_over_dc(dcB, false),
    ]);
    return { fpA, fpB };
  }, WASM);

  // Both handshakes completed → mutual auth succeeded over the real data channel.
  expect(result.fpA).toMatch(/^[0-9a-f]+$/);
  expect(result.fpB).toMatch(/^[0-9a-f]+$/);
  // Each sees the OTHER's fingerprint, so they differ.
  expect(result.fpA).not.toBe(result.fpB);
});

test("PWA syncs a mesh event over a real RTCDataChannel", async ({ page }) => {
  await page.goto("/");
  const result = await page.evaluate(async (wasmPath) => {
    const mod = await import(/* @vite-ignore */ wasmPath);
    await mod.default();

    const a = new RTCPeerConnection();
    const b = new RTCPeerConnection();
    a.onicecandidate = (e) => e.candidate && b.addIceCandidate(e.candidate);
    b.onicecandidate = (e) => e.candidate && a.addIceCandidate(e.candidate);

    const dcA = a.createDataChannel("mesh");
    const dcBReady = new Promise<RTCDataChannel>((res) => {
      b.ondatachannel = (e) => res(e.channel);
    });
    const openA = new Promise<void>((res) => {
      dcA.onopen = () => res();
    });

    const offer = await a.createOffer();
    await a.setLocalDescription(offer);
    await b.setRemoteDescription(offer);
    const answer = await b.createAnswer();
    await b.setLocalDescription(answer);
    await a.setRemoteDescription(answer);

    const dcB = await dcBReady;
    const openB = new Promise<void>((res) => {
      if (dcB.readyState === "open") res();
      else dcB.onopen = () => res();
    });
    await Promise.all([openA, openB]);

    // A (initiator) is seeded with one event; B (responder) starts empty. After the sync round
    // over the data channel, both stores should hold the one event.
    const [countA, countB] = await Promise.all([
      mod.sync_demo_over_dc(dcA, true, true),
      mod.sync_demo_over_dc(dcB, false, false),
    ]);
    return { countA, countB };
  }, WASM);

  expect(result.countA).toBe(1); // initiator kept its seeded event
  expect(result.countB).toBe(1); // responder received it over the data channel
});
