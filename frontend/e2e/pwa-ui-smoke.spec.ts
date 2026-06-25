import { test, expect, type Page } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

// End-to-end UI smoke over the REAL stack: a real signaling relay (mesh-talk-signal) + two real
// browser PWAs (real wasm nodes, real WebRTC, real Double-Ratchet DMs). Unlike pwa-two-peers (which
// drives messaging through the backend), this drives the ACTUAL UI: each peer auto-meshes from a
// `?relay=…&room=…` URL (useChatRuntime), then one peer types in the composer and the OTHER peer
// sees the message bubble appear in its own UI. The full business path, clicked like a human.
test.describe("real-stack UI smoke (two browsers over a live relay)", () => {
  test.describe.configure({ retries: 2 });

  const PORT = 47995;
  const URL = `ws://127.0.0.1:${PORT}`;
  const ROOM = "ui-smoke";
  const dir = path.dirname(fileURLToPath(import.meta.url));
  const BIN = path.resolve(dir, "../../target/release/mesh-talk-signal");
  let relay: ChildProcess;

  test.skip(!existsSync(BIN), "mesh-talk-signal release binary not built");
  test.skip(
    !process.env.CI && !process.env.RUN_GATEWAY_E2E,
    "heavy mesh e2e — CI / RUN_GATEWAY_E2E only",
  );

  test.beforeAll(async () => {
    relay = spawn(BIN, ["--host", "127.0.0.1", "--port", String(PORT)], {
      stdio: "ignore",
    });
    await new Promise((r) => setTimeout(r, 2000));
  });
  test.afterAll(() => relay?.kill());

  // Log in via the real UI. No relay in the URL — meshing is started explicitly AFTER both peers
  // are logged in, because the wasm PBKDF2 login is synchronous + CPU-heavy and a concurrent mesh
  // loop would starve the second peer's login (the form's submit stays disabled).
  async function signIn(page: Page, name: string) {
    await page.goto("/");
    for (const tab of ["register", "signin"]) {
      await page.getByTestId(`login-tab-${tab}`).click();
      await page.getByTestId("login-username").fill(name);
      await page.getByTestId("login-password").fill("password123");
      await page.getByTestId("login-submit").click();
    }
    await expect(page.getByTestId("chat-shell")).toBeVisible({
      timeout: 45_000,
    });
  }

  const myId = (page: Page) =>
    page.evaluate(async () => {
      const m = (await import("/src/lib/browserBackend.ts")) as {
        browserInvoke: (c: string) => Promise<unknown>;
      };
      return (await m.browserInvoke("my_id")) as string;
    });

  // Start the real continuous mesh sync loop inside the page — the same transport the UI runs on a
  // configured relay. It delivers presence + conversations into the local wasm node; the store's
  // periodic roster interval then surfaces peers in the sidebar, and re-opening a conversation
  // (open() → reload) reads the freshly-synced messages from the local node.
  const startMesh = (page: Page) =>
    page.evaluate(
      async ({ url, room }) => {
        const m = (await import("/src/lib/browserBackend.ts")) as {
          runMeshSync: (u: string[], r: string) => () => void;
        };
        (window as unknown as { __stopMesh?: () => void }).__stopMesh =
          m.runMeshSync([url], room);
      },
      { url: URL, room: ROOM },
    );

  test("two PWAs mesh, then DM each other through the composer UI", async ({
    browser,
  }) => {
    // Generous headroom: two receive directions each poll up to 60s, plus logins + mesh setup.
    test.setTimeout(180_000);
    const ctxA = await browser.newContext();
    const ctxB = await browser.newContext();
    const A = await ctxA.newPage();
    const B = await ctxB.newPage();

    // Sequential UI logins (no mesh yet, so neither login is starved).
    await signIn(A, "alice");
    await signIn(B, "bob");

    const aId = await myId(A);
    const bId = await myId(B);

    // Now start the real mesh on both (A first → hub, B → spoke). Transport only; the UI updates
    // via the store's roster interval + open()-reload.
    await startMesh(A);
    await new Promise((r) => setTimeout(r, 800));
    await startMesh(B);

    // A's sidebar surfaces Bob via gossiped presence.
    const aRow = A.getByTestId(`conversation-row-${bId}`);
    await expect(aRow).toBeVisible({ timeout: 90_000 });

    // A opens Bob and sends a message THROUGH THE UI (click row, type, send) — A sees its bubble.
    await aRow.click();
    await expect(A.getByTestId("conversation-header")).toBeVisible();
    await A.getByTestId("composer-input").fill("hello bob, from the UI");
    await A.getByTestId("composer-send").click();
    await expect(A.getByTestId("message-bubble").last()).toBeVisible();

    // Re-open the peer's conversation each poll so the UI reloads from the local node as the mesh
    // delivers new messages — assert the bubble text shows up in the OTHER peer's real UI.
    const seesBubble = (reader: Page, peerId: string, text: string) =>
      expect
        .poll(
          async () => {
            const row = reader.getByTestId(`conversation-row-${peerId}`);
            if (await row.isVisible().catch(() => false)) await row.click();
            return reader
              .getByTestId("message-bubble")
              .filter({ hasText: text })
              .count()
              .catch(() => 0);
          },
          { timeout: 60_000, intervals: [1500] },
        )
        .toBeGreaterThan(0);

    // Bob's UI receives + decrypts + renders Alice's message (real WebRTC).
    await seesBubble(B, aId, "hello bob, from the UI");

    // Reply direction, also through the UI: Bob → Alice.
    await B.getByTestId("composer-input").fill("hi alice, got it");
    await B.getByTestId("composer-send").click();
    await seesBubble(A, bId, "hi alice, got it");

    await ctxA.close();
    await ctxB.close();
  });
});
