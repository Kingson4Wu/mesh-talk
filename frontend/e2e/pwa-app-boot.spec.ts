import { test, expect } from "@playwright/test";

// The React app boots in browser mode (NO Tauri) against the wasm backend (mobile-PWA plan,
// Phase 2): register + sign in run against the persistent wasm keystore, and the chat shell
// renders (empty roster — no discovery/gateway peers yet). Proves the PWA runs the real app with
// a durable identity; messaging-over-the-gateway wiring is the next step.
test.setTimeout(40_000);

test("PWA boots into the app with a wasm-backed identity", async ({ page }) => {
  await page.goto("/");

  // Register, then sign in — both open the wasm keystore (same password → same identity).
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("pwauser");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }

  // The app reached its main shell, driven by the wasm node (my_id/account_id), with an empty
  // conversation list (browser has no peers yet).
  await expect(page.getByTestId("chat-shell")).toBeVisible({ timeout: 30_000 });
});
