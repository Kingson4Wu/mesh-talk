import { test, expect } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

// THE reported scenario, end to end with REAL components (no mocks, no synthetic rosters):
//   PC2  — a real desktop node on the "LAN" (loopback UDP discovery)
//   PC1  — a real desktop node on the SAME LAN that ALSO hosts the gateway hub on the relay
//   phone — a real browser PWA that connects only to the relay
// The phone never connects to PC2. It must still see PC2, because PC1 learns PC2 over the LAN
// (directory gossip) and re-gossips it to the phone over the relay. This also exercises the real
// run_mesh_hub (Rust hub) ↔ runMeshSync (browser spoke) path that browser-only tests never cover.
test.describe("phone sees a LAN PC via a desktop gateway hub (real nodes + relay)", () => {
  test.describe.configure({ retries: 2 });

  const dir = path.dirname(fileURLToPath(import.meta.url));
  const RELAY_BIN = path.resolve(dir, "../../target/release/mesh-talk-signal");
  // The REAL desktop runtime, headless — the exact NodeRuntime code path the Tauri app runs
  // (discovery + presence gossip + run_gateway_hub), not the simpler mesh-talk-node REPL.
  const NODE_BIN = path.resolve(dir, "../../target/release/mesh-talk-runtime-node");
  const PORT = 47997;
  const URL = `ws://127.0.0.1:${PORT}`;
  const ROOM = "lan-bridge";
  const DP = 47010; // a non-default discovery port (loopback LAN), so the test is isolated

  const procs: ChildProcess[] = [];
  const out = new Map<ChildProcess, string[]>();
  function track(p: ChildProcess) {
    procs.push(p);
    const buf: string[] = [];
    out.set(p, buf);
    let acc = "";
    p.stdout?.on("data", (d) => {
      acc += d.toString();
      const parts = acc.split("\n");
      acc = parts.pop() ?? "";
      for (const l of parts) buf.push(l);
    });
  }
  const waitLine = (p: ChildProcess, re: RegExp, timeoutMs = 30000) =>
    new Promise<RegExpMatchArray>((resolve, reject) => {
      const t0 = Date.now();
      const iv = setInterval(() => {
        for (const l of out.get(p) ?? []) {
          const m = l.match(re);
          if (m) {
            clearInterval(iv);
            resolve(m);
            return;
          }
        }
        if (Date.now() - t0 > timeoutMs) {
          clearInterval(iv);
          reject(new Error(`timeout waiting for ${re}`));
        }
      }, 200);
    });

  const node = (name: string, ...extra: string[]) => {
    const d = mkdtempSync(path.join(tmpdir(), `${name}-`));
    const p = spawn(
      NODE_BIN,
      [
        "--data-dir",
        d,
        "--account-id",
        name,
        "--name",
        name,
        "--password",
        "pw",
        "--discovery-port",
        String(DP),
        ...extra,
      ],
      { stdio: ["pipe", "pipe", "ignore"] },
    );
    track(p);
    return p;
  };

  test.skip(
    !existsSync(RELAY_BIN) || !existsSync(NODE_BIN),
    "release binaries not built (cargo build -p mesh-talk-core --bin mesh-talk-node --features gateway,fast-test-kdf --release)",
  );
  test.skip(
    !process.env.CI && !process.env.RUN_GATEWAY_E2E,
    "heavy mesh e2e — CI / RUN_GATEWAY_E2E only",
  );

  test.afterAll(() => {
    for (const p of procs) p.kill();
  });

  test("a phone reaches a LAN PC it never connected to, via a desktop gateway hub", async ({
    browser,
  }) => {
    test.setTimeout(180_000);

    // 1) the relay.
    const relay = spawn(RELAY_BIN, ["--host", "127.0.0.1", "--port", String(PORT)], {
      stdio: "ignore",
    });
    procs.push(relay);
    await new Promise((r) => setTimeout(r, 1500));

    // 2) PC2 — a plain LAN desktop node.
    const pc2 = node("PC2-Desktop");
    const pc2Id = (await waitLine(pc2, /node ([0-9a-f]{32}) listening/))[1];

    // 3) PC1 — a LAN desktop node that ALSO hosts the gateway hub.
    const pc1 = node("PC1-Hub", "--signal-url", URL, "--gateway-room", ROOM);
    await waitLine(pc1, /node ([0-9a-f]{32}) listening/);

    // 4) PC1 must learn PC2 over the LAN (discovery + directory gossip) — this is the desktop-side
    //    link. Poll PC1's /dir until its presence directory carries PC2.
    await expect
      .poll(
        async () => {
          pc1.stdin?.write("/dir\n");
          await new Promise((r) => setTimeout(r, 600));
          return (out.get(pc1) ?? []).some((l) => l.startsWith(`dir ${pc2Id} `));
        },
        { timeout: 60_000, intervals: [2000] },
      )
      .toBe(true);

    // 5) the phone: sign in on a relay-configured URL so it auto-meshes with PC1 (the hub).
    const ctx = await browser.newContext();
    const P = await ctx.newPage();
    await P.goto(`/?relay=${encodeURIComponent(URL)}&room=${ROOM}`);
    for (const tab of ["register", "signin"]) {
      await P.getByTestId(`login-tab-${tab}`).click();
      await P.getByTestId("login-username").fill("phone");
      await P.getByTestId("login-password").fill("password123");
      await P.getByTestId("login-submit").click();
    }
    await expect(P.getByTestId("chat-shell")).toBeVisible({ timeout: 45_000 });

    // 6) THE ASSERTION: the phone sees PC2 — a LAN PC it never connected to — in its sidebar,
    //    re-gossiped through PC1's gateway hub.
    await expect(P.getByTestId(`conversation-row-${pc2Id}`)).toBeVisible({
      timeout: 90_000,
    });

    await ctx.close();
  });
});
