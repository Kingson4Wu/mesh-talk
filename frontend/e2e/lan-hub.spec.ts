import { test, expect } from "./tauri-mock";

// Desktop LAN hub UI (mobile-PWA plan, Phase 3): in the Tauri app, Settings has a "LAN sharing"
// section — an "act as relay" toggle and a "let a phone download the app" action that starts the
// http host and shows a QR of the join URL. Drives the mocked Tauri commands.
test("Settings exposes the LAN hub: relay toggle + share-app QR", async ({
  page,
}) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();

  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-settings").click();
  await expect(page.getByTestId("settings-dialog")).toBeVisible();

  // The relay toggle is present and flips (calls set_relay_running, mocked ok).
  const relay = page.getByTestId("settings-relay-toggle");
  await expect(relay).toBeVisible();
  await relay.click();

  // Sharing the app starts the host and shows a QR of the join URL.
  await page.getByTestId("settings-share-app").click();
  await expect(page.getByTestId("settings-share-qr")).toBeVisible();
  await expect(page.getByText("192.168.1.10:8080")).toBeVisible();
});
