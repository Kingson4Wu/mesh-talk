import { test as base, expect } from "@playwright/test";

/**
 * Headless E2E runs the React frontend in plain Chromium — there is no Tauri runtime, so
 * `@tauri-apps/api` (invoke/listen) would throw ("Cannot read properties of undefined
 * (reading 'transformCallback')"). This fixture injects a minimal mock of
 * `window.__TAURI_INTERNALS__` with canned command responses, so the full UI flow
 * (register → login → render → open conversation → send) runs without the Rust backend.
 *
 * The real backend is covered separately by the Rust multi-process suite (`make e2e`); this
 * suite guards the UI rendering + wiring (the layer that black-screened v0.1.0).
 */
export const test = base.extend({
  page: async ({ page }, provide) => {
    await page.addInitScript(() => {
      const SELF = { device: "device_self", account: "acc_self" };
      const CONTACT = {
        account_id: "acc_bob",
        device_count: 1,
        names: ["bob"],
      };
      const ok = () => null;
      // Stateful message store so a sent message survives the post-send reload() (which fetches
      // the real id) — mirroring how the real backend persists it.
      const msgs: Record<string, Array<Record<string, unknown>>> = {};
      let mid = 0;
      const record = (conv: string, text: string, replyTo: unknown) => {
        (msgs[conv] ||= []).push({
          id: `m${++mid}`,
          from_me: true,
          who: SELF.device,
          text,
          wall_clock: 1_700_000_000_000 + mid,
          reply_to: (replyTo as string) ?? null,
        });
        return null;
      };
      const responses: Record<string, (a: Record<string, unknown>) => unknown> =
        {
          register: () => ({
            success: true,
            user: { id: "u_self", username: "tester" },
          }),
          login: (a) => ({
            success: true,
            user: { id: "u_self", username: String(a.username) },
          }),
          logout: () => ({ success: true }),
          adopt_linked_account: ok,
          my_id: () => SELF.device,
          account_id: () => SELF.account,
          list_peers: () => [],
          list_accounts: () => [CONTACT],
          list_channels: () => [],
          history: (a) => msgs[`dm:${a.peer}`] ?? [],
          account_history: (a) => msgs[`acc:${a.account}`] ?? [],
          channel_history: (a) => msgs[`ch:${a.channelId}`] ?? [],
          reactions: () => [],
          account_reactions: () => [],
          channel_reactions: () => [],
          channel_members: () => [],
          search: () => [],
          send_dm: (a) =>
            record(`dm:${a.recipient}`, String(a.text), a.replyTo),
          send_to_account: (a) =>
            record(`acc:${a.account}`, String(a.text), a.replyTo),
          send_channel_message: (a) =>
            record(`ch:${a.channelId}`, String(a.text), a.replyTo),
          react_dm: ok,
          react_account: ok,
          react_channel: ok,
          create_channel: () => "chan_new",
          add_channel_member: ok,
          remove_channel_member: ok,
          save_file: ok,
          start_linking: () => "123456",
          stop_linking: ok,
          rekey_account: () => "rekeyed",
        };
      Object.defineProperty(window, "__TAURI_INTERNALS__", {
        value: {
          transformCallback(cb: (p: unknown) => void) {
            const id = Math.floor(Math.random() * 1e9);
            (window as unknown as Record<string, unknown>)[`_${id}`] = cb;
            return id;
          },
          async invoke(cmd: string, args: Record<string, unknown>) {
            if (cmd === "plugin:event|listen")
              return Math.floor(Math.random() * 1e9);
            if (cmd === "plugin:event|unlisten") return null;
            const fn = responses[cmd];
            return fn ? fn(args || {}) : null;
          },
          convertFileSrc: (p: string) => p,
        },
        configurable: true,
      });
    });
    await provide(page);
  },
});

export { expect };
