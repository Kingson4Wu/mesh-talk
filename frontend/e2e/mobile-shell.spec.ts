import { test, expect } from "./tauri-mock";

// The phone shell (MobileApp) is single-pane: the conversation list, OR the open conversation —
// never both. Below the 768px breakpoint App.tsx renders MobileApp instead of the desktop two-pane.
// This is the first e2e below that breakpoint (the other specs run at ≥820px = desktop tree), so it
// locks the mobile nav flow + the shared testid contract (conversation-row/-header/-back).
const BOB = "acc_bob_bbbb2222";

test.use({ viewport: { width: 390, height: 844 } });

test("phone single-pane: list → open → send → back", async ({ page }) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();

  // List pane: the row shows; no conversation is open yet (single-pane → no header).
  const row = page.getByTestId(`conversation-row-${BOB}`);
  await expect(row).toBeVisible();
  await expect(page.getByTestId("conversation-header")).toHaveCount(0);

  // Tap a row → the conversation screen replaces the list (back button present, list gone).
  await row.click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await expect(page.getByTestId("conversation-back")).toBeVisible();
  await expect(page.getByTestId(`conversation-row-${BOB}`)).toHaveCount(0);

  // Send a message — the shared MessageStream/Composer render exactly as on desktop.
  await page.getByTestId("composer-input").fill("hi from a phone");
  await page.getByTestId("composer-send").click();
  await expect(page.getByTestId("message-bubble").last()).toBeVisible();

  // Back → returns to the single-pane list.
  await page.getByTestId("conversation-back").click();
  await expect(page.getByTestId(`conversation-row-${BOB}`)).toBeVisible();
  await expect(page.getByTestId("conversation-header")).toHaveCount(0);
});
