import { defineConfig, devices } from "@playwright/test";

// Browser E2E for the Mesh-Talk UI. Runs the Vite dev server + drives the real React app in
// headless Chromium with a mocked Tauri IPC layer (see e2e/tauri-mock.ts).
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  // Allow one retry everywhere: the PWA tests do real wasm PBKDF2 logins (synchronous, CPU-heavy)
  // and the multi-context gateway tests add WebRTC, so a round can occasionally exceed its budget
  // under load (e.g. the pre-commit gate, right after clippy/tests). A retry absorbs that flake; a
  // genuine break still fails both attempts.
  retries: 1,
  // Serial: the PWA tests each run a real wasm PBKDF2 login (synchronous on the main thread), so
  // running them in parallel saturates the CPU and makes the login flow flake. Serial is a touch
  // slower but reliable (CI already used 1 worker).
  workers: 1,
  reporter: process.env.CI ? "list" : "html",
  use: {
    baseURL: "http://localhost:5173",
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "npm run dev",
    url: "http://localhost:5173",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
