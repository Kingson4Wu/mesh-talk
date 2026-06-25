import { test, expect } from "@playwright/test";

// "Stay signed in" for the PWA: after a login the session is persisted in IndexedDB, so a reload
// AUTO-unlocks the SAME identity instead of re-showing the login screen — fixing "every login a new
// id / always have to log in". (Same-origin only; browser storage can't cross to another PC's page.)
test.setTimeout(60_000);

const BACKEND = "/src/lib/" + "browserBackend.ts";
const myId = (page: import("@playwright/test").Page) =>
  page.evaluate(async (mod) => {
    const m = (await import(/* @vite-ignore */ mod)) as {
      browserInvoke: (c: string) => Promise<unknown>;
    };
    return (await m.browserInvoke("my_id")) as string;
  }, BACKEND);

test("the PWA stays signed in across a reload (same identity, no re-login)", async ({
  page,
}) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("kingson");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible({ timeout: 45_000 });
  const id1 = await myId(page);

  // Reload → must AUTO-login (no login screen) to the SAME identity.
  await page.reload();
  await expect(page.getByTestId("chat-shell")).toBeVisible({ timeout: 45_000 });
  await expect(page.getByTestId("login-submit")).toHaveCount(0);
  expect(await myId(page)).toBe(id1);
});
