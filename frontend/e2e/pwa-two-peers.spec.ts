import { test, expect, type Page } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Full mesh integration flow (mobile-PWA plan, Phase 3), end to end over real WebRTC:
// a real signaling relay (the mesh-talk-signal binary) + three PWA browser contexts — a hub and
// two phones that NEVER connect directly. The phones connect only to the hub; the presence
// directory + conversations gossip through it, so one phone discovers the other and an
// account-addressed, end-to-end-encrypted (Double Ratchet) DM routes A → hub → B and decrypts.
// This is the "one mesh, every node reaches every node, phones via a hub" model, proven.
//
// One focused integration test (not a back-to-back suite): multi-context WebRTC + wasm PBKDF2
// logins are heavy, so a single comprehensive flow stays reliable where several in a row flake.
test.describe("full mesh flow over the gateway", () => {
  // These tests drive 3–4 browser contexts + real WebRTC + several wasm PBKDF2 logins each, so a
  // first attempt under load can time out; retries keep them dependably green (they pass first-try
  // on their own).
  test.describe.configure({ retries: 2 });

  const PORT = 47991;
  const URL = `ws://127.0.0.1:${PORT}`;
  const dir = path.dirname(fileURLToPath(import.meta.url));
  const BIN = path.resolve(dir, "../../target/release/mesh-talk-signal");
  let relay: ChildProcess;

  // The relay is a native binary built separately (cargo build -p mesh-talk-signal --release);
  // skip cleanly where it isn't present (e.g. a frontend-only CI lane) rather than fail.
  test.skip(!existsSync(BIN), "mesh-talk-signal release binary not built");
  // Heavy multi-context WebRTC: run in CI (which builds the relay) + on demand, but skip in the
  // FAST local pre-commit gate (several of these back-to-back blow past its time budget). Run
  // locally with: RUN_GATEWAY_E2E=1 npx playwright test pwa-two-peers
  test.skip(
    !process.env.CI && !process.env.RUN_GATEWAY_E2E,
    "heavy mesh e2e — CI / RUN_GATEWAY_E2E only",
  );

  test.beforeAll(async () => {
    relay = spawn(BIN, ["--host", "127.0.0.1", "--port", String(PORT)], {
      stdio: "ignore",
    });
    await new Promise((r) => setTimeout(r, 2000)); // let it bind
  });
  test.afterAll(() => relay?.kill());

  const BACKEND = "/src/lib/" + "browserBackend.ts";

  async function signIn(page: Page, name: string) {
    await page.goto("/");
    for (const tab of ["register", "signin"]) {
      await page.getByTestId(`login-tab-${tab}`).click();
      await page.getByTestId("login-username").fill(name);
      await page.getByTestId("login-password").fill("password123");
      await page.getByTestId("login-submit").click();
    }
    await expect(page.getByTestId("chat-shell")).toBeVisible({ timeout: 45_000 });
  }

  // Call a browserBackend export inside the page (the same module instance the app uses).
  const call = (page: Page, fn: string, args: unknown[] = []) =>
    page.evaluate(
      async ({ mod, fn, args }) => {
        const m = (await import(/* @vite-ignore */ mod)) as Record<
          string,
          (...a: unknown[]) => unknown
        >;
        return await m[fn](...args);
      },
      { mod: BACKEND, fn, args },
    );

  test("a phone reaches a peer it never met directly, via a hub (encrypted)", async ({
    browser,
  }) => {
    test.setTimeout(120_000);
    const ctxA = await browser.newContext();
    const ctxH = await browser.newContext();
    const ctxB = await browser.newContext();
    const A = await ctxA.newPage();
    const H = await ctxH.newPage();
    const B = await ctxB.newPage();
    await signIn(A, "amy");
    await signIn(H, "hub");
    await signIn(B, "ben");

    // One gossip round between the hub and a phone. The hub joins first (responder); the phone
    // joins second (initiator) so the phone pushes/pulls all its conversations through the hub.
    const gossip = async (phone: typeof A, room: string) => {
      const hub = call(H, "connectGossipSync", [URL, room]);
      await new Promise((r) => setTimeout(r, 300));
      await call(phone, "connectGossipSync", [URL, room]);
      await hub;
    };

    const aId = (await call(A, "browserInvoke", ["my_id"])) as string;
    const bId = (await call(B, "browserInvoke", ["my_id"])) as string;

    // Propagate the presence directory through the hub: hub←A, hub←B (B learns A), A learns B.
    await gossip(A, "ra");
    await gossip(B, "rb");
    await gossip(A, "ra");

    // A discovered B via the hub's gossiped directory — they never connected directly.
    const peers = (await call(A, "browserInvoke", ["list_peers"])) as {
      account_id: string;
    }[];
    expect(peers.map((p) => p.account_id)).toContain(bId);

    // A sends B an account-addressed, end-to-end-encrypted DM; it routes A → hub → B by gossip.
    await call(A, "browserInvoke", [
      "send_to_account",
      { account: bId, text: "mesh hello" },
    ]);
    await gossip(A, "ra"); // hub pulls conv(A,B) from A
    await gossip(B, "rb"); // B pulls conv(A,B) from the hub + decrypts it

    const hist = (await call(B, "browserInvoke", [
      "account_history",
      { account: aId, limit: 200 },
    ])) as { text: string }[];
    expect(hist.map((x) => x.text)).toContain("mesh hello");

    await ctxA.close();
    await ctxH.close();
    await ctxB.close();
  });

  // Start the continuous mesh sync loop inside a page (the hub-or-spoke participant). Returns
  // nothing (the stop fn isn't serializable); the loop runs until the context closes.
  const startMesh = (page: Page, room: string, relays: string[] = [URL]) =>
    page.evaluate(
      async ({ mod, relays, room }) => {
        const m = (await import(/* @vite-ignore */ mod)) as {
          runMeshSync: (u: string[], r: string) => () => void;
        };
        (window as unknown as { __stopMesh?: () => void }).__stopMesh =
          m.runMeshSync(relays, room);
      },
      { mod: BACKEND, relays, room },
    );

  test("many phones chat through ONE hub in one room (multi-peer mesh)", async ({
    browser,
  }) => {
    test.setTimeout(180_000);
    // A hub + THREE phones, all in the SAME room — exercising the relay's multi-peer mode
    // (no 2-peer limit). The hub holds a gossiping connection to each phone.
    const ctxs = await Promise.all(
      [0, 1, 2, 3].map(() => browser.newContext()),
    );
    const [H, A, B, C] = await Promise.all(ctxs.map((c) => c.newPage()));
    // Sequential sign-in: concurrent wasm PBKDF2 logins contend and time out.
    await signIn(H, "hub");
    await signIn(A, "ada");
    await signIn(B, "bea");
    await signIn(C, "cam");

    const room = "mesh-many";
    // The hub joins first (so it becomes the hub), then the three phones.
    await startMesh(H, room);
    await new Promise((r) => setTimeout(r, 800));
    await Promise.all([
      startMesh(A, room),
      startMesh(B, room),
      startMesh(C, room),
    ]);

    const id = async (p: Page) =>
      (await call(p, "browserInvoke", ["my_id"])) as string;
    const [aId, bId, cId] = [await id(A), await id(B), await id(C)];

    // Ada discovers BOTH other phones via the hub's gossiped directory (never connecting to them
    // directly). Poll: discovery propagates over a few gossip rounds.
    await expect
      .poll(
        async () => {
          const peers = (await call(A, "browserInvoke", ["list_peers"])) as {
            account_id: string;
          }[];
          const ids = peers.map((p) => p.account_id);
          return ids.includes(bId) && ids.includes(cId);
        },
        { timeout: 90_000, intervals: [1000] },
      )
      .toBe(true);

    // Ada sends an encrypted DM to each — they route Ada → hub → phone.
    await call(A, "browserInvoke", [
      "send_to_account",
      { account: bId, text: "hi bea" },
    ]);
    await call(A, "browserInvoke", [
      "send_to_account",
      { account: cId, text: "hi cam" },
    ]);

    // Both phones receive + decrypt Ada's message (through the hub, not from Ada directly).
    const gotFromAda = (p: Page, text: string) =>
      expect
        .poll(
          async () => {
            const h = (await call(p, "browserInvoke", [
              "account_history",
              { account: aId, limit: 200 },
            ])) as { text: string }[];
            return h.map((x) => x.text).includes(text);
          },
          { timeout: 90_000, intervals: [1000] },
        )
        .toBe(true);
    await gotFromAda(B, "hi bea");
    await gotFromAda(C, "hi cam");

    await Promise.all(ctxs.map((c) => c.close()));
  });

  test("a phone fails over to another relay when its first one is down", async ({
    browser,
  }) => {
    test.setTimeout(120_000);
    // The phone's first relay is dead (nothing listening); it must fall back to the live one and
    // still mesh with the hub there — the "if the scanned computer goes offline" resilience.
    const dead = `ws://127.0.0.1:${PORT + 7}`;
    const room = "failover";
    const ch = await browser.newContext();
    const cp = await browser.newContext();
    const H = await ch.newPage();
    const P = await cp.newPage();
    await signIn(H, "hubz");
    await signIn(P, "phonez");

    // Hub on the LIVE relay; phone tries the DEAD relay first, then the live one.
    await startMesh(H, room, [URL]);
    await new Promise((r) => setTimeout(r, 800));
    await startMesh(P, room, [dead, URL]);

    const hubId = (await call(H, "browserInvoke", ["my_id"])) as string;
    await expect
      .poll(
        async () => {
          const peers = (await call(P, "browserInvoke", ["list_peers"])) as {
            account_id: string;
          }[];
          return peers.some((p) => p.account_id === hubId);
        },
        { timeout: 90_000, intervals: [1000] },
      )
      .toBe(true);

    await ch.close();
    await cp.close();
  });

  test("a phone learns another relay through gossip (zero-config failover discovery)", async ({
    browser,
  }) => {
    test.setTimeout(120_000);
    // A hub knows an extra relay endpoint (as a desktop announces its own / it bridges in over the
    // LAN mesh). A phone that meshes with the hub must LEARN that relay via gossip — so it could
    // fail over to a relay it never scanned. The basis for zero-config failover.
    const otherRelay = "ws://198.51.100.7:47480";
    const room = "relaydir";
    const ch = await browser.newContext();
    const cp = await browser.newContext();
    const H = await ch.newPage();
    const P = await cp.newPage();
    await signIn(H, "hubr");
    await signIn(P, "phoner");

    await call(H, "announceRelay", [otherRelay]);
    await startMesh(H, room, [URL]);
    await new Promise((r) => setTimeout(r, 800));
    await startMesh(P, room, [URL]);

    await expect
      .poll(
        async () => {
          const relays = (await call(P, "meshKnownRelays")) as string[];
          return relays.includes(otherRelay);
        },
        { timeout: 90_000, intervals: [1000] },
      )
      .toBe(true);

    await ch.close();
    await cp.close();
  });

  test("a phone and the hub DM each other directly, both directions", async ({
    browser,
  }) => {
    // The real desktop↔phone case: the desktop IS the hub, and the phone DMs it directly (not
    // another spoke via the hub). The hub→phone direction was broken — the phone never requested
    // the DM conversation with the hub it just connected to. Both directions must deliver.
    test.setTimeout(120_000);
    const ctxH = await browser.newContext();
    const ctxA = await browser.newContext();
    const H = await ctxH.newPage();
    const A = await ctxA.newPage();
    await signIn(H, "deskhub"); // the "desktop" (hub)
    await signIn(A, "thephone"); // the phone (spoke)

    const room = "hubdm";
    // Hub joins first (responder); phone joins second (initiator) — the real topology.
    const gossip = async () => {
      const hub = call(H, "connectGossipSync", [URL, room]);
      await new Promise((r) => setTimeout(r, 300));
      await call(A, "connectGossipSync", [URL, room]);
      await hub;
    };

    const hId = (await call(H, "browserInvoke", ["my_id"])) as string;
    const aId = (await call(A, "browserInvoke", ["my_id"])) as string;
    await gossip();
    await gossip(); // let presence propagate both ways

    // A message arrives once it has had enough gossip rounds to converge — poll (gossip + check)
    // rather than assuming a fixed number, since real syncs run continuously.
    const deliversTo = async (reader: typeof A, fromId: string, text: string) =>
      expect
        .poll(
          async () => {
            await gossip();
            const hist = (await call(reader, "browserInvoke", [
              "account_history",
              { account: fromId, limit: 200 },
            ])) as { text: string }[];
            return hist.map((x) => x.text);
          },
          { timeout: 30_000 },
        )
        .toContain(text);

    // Phone → hub.
    await call(A, "browserInvoke", [
      "send_to_account",
      { account: hId, text: "phone to hub" },
    ]);
    await deliversTo(H, aId, "phone to hub");

    // Hub → phone (the previously-broken direction: the phone must request dm(phone, hub)).
    await call(H, "browserInvoke", [
      "send_to_account",
      { account: aId, text: "hub to phone" },
    ]);
    await deliversTo(A, hId, "hub to phone");

    await ctxH.close();
    await ctxA.close();
  });
});
