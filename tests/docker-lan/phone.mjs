// Runs inside a Playwright container ON the bridge network — the "phone". Loads the PWA served by
// the relay, logs in, and asserts it sees PC2 (a LAN desktop it never connected to), bridged
// through PC1's gateway hub. Exits non-zero (fails the test) if PC2 never appears.
import { chromium } from "playwright";

const PWA_URL = process.env.PWA_URL;
const PC2_ID = process.env.PC2_ID;
if (!PWA_URL || !PC2_ID) {
  console.error("missing PWA_URL / PC2_ID");
  process.exit(2);
}

const browser = await chromium.launch({ args: ["--no-sandbox"] });
try {
  const page = await browser.newPage();
  page.on("console", (m) => console.log("[phone-console]", m.text()));
  await page.goto(PWA_URL, { waitUntil: "domcontentloaded" });
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("phone");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await page.getByTestId("chat-shell").waitFor({ timeout: 45_000 });
  console.log("phone: logged in, meshing…");
  await page
    .getByTestId(`conversation-row-${PC2_ID}`)
    .waitFor({ state: "visible", timeout: 120_000 });
  console.log("PHONE-SEES-PC2 OK");
  await browser.close();
  process.exit(0);
} catch (e) {
  console.error("PHONE-FAILED:", e?.message ?? e);
  await browser.close();
  process.exit(1);
}
