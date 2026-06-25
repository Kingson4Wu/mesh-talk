import { test, expect } from "@playwright/test";

// The PWA node's event log persists across a reload via IndexedDB (mobile-PWA plan, Phase 2):
// send a message, reload the page (fresh wasm instance), and the message is still there — the
// browser analog of the desktop node's durable on-disk log.
test.setTimeout(30_000);

const NODE = "/src/lib/" + "pwaNode.ts";

test("PWA node log survives a reload (IndexedDB)", async ({ page }) => {
  await page.goto("/");

  const before = await page.evaluate(async (mod) => {
    const { loadNode, persistNode } = await import(/* @vite-ignore */ mod);
    const node = await loadNode("correct horse");
    node.send_message("survives reload");
    await persistNode(node);
    return { fp: node.fingerprint(), msgs: JSON.parse(node.messages()) as string[] };
  }, NODE);
  expect(before.msgs).toEqual(["survives reload"]);

  await page.reload();

  const after = await page.evaluate(async (mod) => {
    const { loadNode } = await import(/* @vite-ignore */ mod);
    const node = await loadNode("correct horse");
    return { fp: node.fingerprint(), msgs: JSON.parse(node.messages()) as string[] };
  }, NODE);
  expect(after.msgs).toEqual(["survives reload"]); // log restored from IndexedDB
  expect(after.fp).toBe(before.fp); // same identity (keystore restored from IndexedDB)
});
