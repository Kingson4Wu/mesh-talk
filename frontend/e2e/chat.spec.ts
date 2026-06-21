import { test, expect } from "./tauri-mock";

async function registerAndSignIn(
  page: import("@playwright/test").Page,
  user: string,
) {
  await page.goto("/");
  await page.getByRole("tab", { name: "Register" }).click();
  await page.locator("#username").fill(user);
  await page.locator("#password").fill("password123");
  await page.getByRole("button", { name: "Create account" }).click();
  await expect(page.getByText("Account created")).toBeVisible();
  await page.locator("#username").fill(user);
  await page.locator("#password").fill("password123");
  await page.getByRole("button", { name: "Sign in" }).click();
}

test.describe("Mesh-Talk UI flow", () => {
  test("register → sign in → chat renders → send a message", async ({
    page,
  }) => {
    await registerAndSignIn(page, "tester");

    // Chat shell renders (the layer that black-screened v0.1.0).
    await expect(page.getByText("Direct messages")).toBeVisible();
    await expect(page.getByText("No conversation selected")).toBeVisible();

    // The seeded contact is listed; open it and send a message.
    const contact = page.getByRole("button", { name: /bob/ });
    await expect(contact).toBeVisible();
    await contact.click();

    // Wait for the conversation to finish loading (history fetch resolved) before sending —
    // otherwise the late reload() can overwrite the optimistic bubble.
    await expect(page.getByText("No messages yet — say hello.")).toBeVisible();

    const box = page.getByRole("textbox");
    await box.fill("hello e2e");
    await box.press("Enter");
    await expect(page.getByText("hello e2e")).toBeVisible();
  });

  test("post-login view never collapses to a blank/black screen", async ({
    page,
  }) => {
    await registerAndSignIn(page, "guard");
    // The infinite-render bug left #root empty; assert it has content...
    await expect(page.locator("#root")).not.toBeEmpty();
    await expect(page.getByText("Direct messages")).toBeVisible();
    // ...and the ErrorBoundary fallback did NOT trip.
    await expect(page.getByText("Something went wrong")).toHaveCount(0);
  });

  test("URLs in messages render as clickable links", async ({ page }) => {
    await registerAndSignIn(page, "tester");
    await page.getByRole("button", { name: /bob/ }).click();
    await expect(page.getByText("No messages yet — say hello.")).toBeVisible();
    const box = page.getByRole("textbox");
    await box.fill("visit https://example.com today");
    await box.press("Enter");
    await expect(
      page.getByRole("link", { name: "https://example.com" }),
    ).toBeVisible();
  });

  test("emoji picker inserts into the composer", async ({ page }) => {
    await registerAndSignIn(page, "tester");
    await page.getByRole("button", { name: /bob/ }).click();
    await expect(page.getByText("No messages yet — say hello.")).toBeVisible();
    await page.getByRole("button", { name: "Emoji" }).click();
    await page.getByRole("button", { name: "Insert 🎉" }).click();
    await expect(page.getByRole("textbox")).toHaveValue("🎉");
  });

  test("theme toggle switches between dark and light", async ({ page }) => {
    await registerAndSignIn(page, "tester");
    await expect(page.getByText("Direct messages")).toBeVisible();
    await expect(page.locator("html")).toHaveClass(/dark/);
    await page.getByRole("button", { name: "Toggle theme" }).click();
    await expect(page.locator("html")).not.toHaveClass(/dark/);
  });

  test("sign out returns to the login screen", async ({ page }) => {
    await registerAndSignIn(page, "tester");
    await expect(page.getByText("Direct messages")).toBeVisible();
    await page.getByRole("button", { name: "Sign out" }).click();
    await expect(page.getByRole("button", { name: "Sign in" })).toBeVisible();
  });
});
