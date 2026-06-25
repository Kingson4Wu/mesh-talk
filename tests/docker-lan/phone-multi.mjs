// Phone in a container: log in, then report which of PC_IDS (comma-separated) it sees in the
// sidebar — i.e. how many LAN PCs the hub relayed to it. Exits 0 if it sees ALL of them.
import { chromium } from "playwright";

const PWA_URL = process.env.PWA_URL;
const IDS = (process.env.PC_IDS || "").split(",").filter(Boolean);
const browser = await chromium.launch({ args: ["--no-sandbox"] });
try {
  const page = await browser.newPage();
  await page.goto(PWA_URL, { waitUntil: "domcontentloaded" });
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("phone");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await page.getByTestId("chat-shell").waitFor({ timeout: 45_000 });
  console.log("phone: logged in, meshing…");

  const seen = new Set();
  const deadline = Date.now() + 90_000;
  while (Date.now() < deadline && seen.size < IDS.length) {
    for (const id of IDS) {
      if (seen.has(id)) continue;
      if (await page.getByTestId(`conversation-row-${id}`).isVisible().catch(() => false)) {
        seen.add(id);
        console.log(`phone SEES ${id}`);
      }
    }
    if (seen.size < IDS.length) await page.waitForTimeout(2000);
  }
  console.log(`phone saw ${seen.size}/${IDS.length} PCs`);
  await browser.close();
  process.exit(seen.size === IDS.length ? 0 : 1);
} catch (e) {
  console.error("PHONE-FAILED:", e?.message ?? e);
  await browser.close();
  process.exit(1);
}
