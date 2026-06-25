import { test, expect } from "@playwright/test";

// Two stateful WasmNodes exchange a real message over a WebRTC data channel (mobile-PWA plan,
// Phase 2). Node A sends a message into its event log, then both nodes sync over the data
// channel — A's message must appear in B's log. This is the PWA's send → sync → receive path
// (the seed of the command surface) running end to end in the browser.
test.setTimeout(30_000);

const WASM = "/src/wasm/" + "mesh_talk_wasm.js";

test("two WasmNodes exchange a message over a data channel", async ({
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

    const nodeA = new mod.WasmNode();
    const nodeB = new mod.WasmNode();
    nodeA.send_message("hello from A");

    // Sync both nodes' logs over the data channel (A initiates).
    await Promise.all([
      nodeA.sync_over_dc(dcA, true),
      nodeB.sync_over_dc(dcB, false),
    ]);

    return {
      a: JSON.parse(nodeA.messages()) as string[],
      b: JSON.parse(nodeB.messages()) as string[],
    };
  }, WASM);

  expect(result.a).toEqual(["hello from A"]);
  expect(result.b).toEqual(["hello from A"]); // received over the data channel
});
