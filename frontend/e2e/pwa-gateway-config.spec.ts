import { test, expect } from "@playwright/test";

// When a gateway relay is configured (localStorage), the PWA starts a background sync loop on
// boot (mobile-PWA plan, Phase 2). This verifies the boot wiring is safe: the app boots cleanly
// into the chat shell with the loop running, and a missing/unreachable relay just retries
// harmlessly (no crash, no console errors). Actual cross-PWA delivery over the loop is covered
// by pwa-two-peers ("continuous background sync").
test("the PWA boots cleanly with a gateway relay configured", async ({
  page,
}) => {
  test.setTimeout(40_000);
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));

  // A relay that isn't listening: the boot sync loop must fail + retry without breaking the app.
  await page.context().addInitScript(() => {
    localStorage.setItem("gateway-relay-url", "ws://127.0.0.1:47990");
    localStorage.setItem("gateway-room", "config-test");
  });

  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("configuser");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }

  await expect(page.getByTestId("chat-shell")).toBeVisible({ timeout: 30_000 });
  // Give the boot loop a couple of failing rounds — it must not surface an error to the page.
  await page.waitForTimeout(1500);
  expect(errors).toEqual([]);
});

test("the relay address can be set from Settings and persists (browser)", async ({
  page,
}) => {
  test.setTimeout(40_000);
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("relayuser");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible({ timeout: 30_000 });

  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-settings").click();
  await expect(page.getByTestId("settings-dialog")).toBeVisible();
  const field = page.getByTestId("settings-gateway-relay");
  await expect(field).toBeVisible();
  await field.fill("ws://relay.example:8080");

  const stored = await page.evaluate(() =>
    localStorage.getItem("gateway-relay-url"),
  );
  expect(stored).toBe("ws://relay.example:8080");

  // With a relay set, the shareable invite QR appears.
  await expect(page.getByTestId("settings-gateway-qr")).toBeVisible();

  // Pasting an invite link applies its relay + room.
  await field.fill(
    "meshtalk://gateway?relay=ws%3A%2F%2Frelay.test%3A9000&room=teamx",
  );
  const cfg = await page.evaluate(() => ({
    relay: localStorage.getItem("gateway-relay-url"),
    room: localStorage.getItem("gateway-room"),
  }));
  expect(cfg.relay).toBe("ws://relay.test:9000");
  expect(cfg.room).toBe("teamx");
});

test("opening a scanned URL (?relay&room) auto-configures the gateway", async ({
  page,
}) => {
  test.setTimeout(40_000);
  // Simulates scanning the desktop hub's QR, which opens the app at this URL.
  await page.goto("/?relay=ws%3A%2F%2F192.168.1.10%3A47800&room=lan-room");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("scanuser");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible({ timeout: 30_000 });

  const cfg = await page.evaluate(() => ({
    relay: localStorage.getItem("gateway-relay-url"),
    room: localStorage.getItem("gateway-room"),
  }));
  expect(cfg.relay).toBe("ws://192.168.1.10:47800");
  expect(cfg.room).toBe("lan-room");
});
