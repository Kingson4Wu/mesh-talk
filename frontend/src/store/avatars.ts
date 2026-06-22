import { create } from "zustand";
import { avatars as avatarsApi } from "@/lib/api";

/**
 * Custom avatars slice. A user can set a local profile photo for themselves or any
 * contact, replacing the deterministic IdentityGlyph wherever that identity renders.
 * Photos are stored LOCALLY (persisted on the Rust side as `avatars.json`); they are
 * never propagated to peers — see the out-of-scope note in `src-tauri/src/avatars.rs`.
 *
 * Kept isolated from the chat store so an avatar change re-renders only the small
 * identity spots, not the (expensive) message list.
 */

interface AvatarsState {
  /** id (account_id / channel_id) -> data-URL string. */
  map: Record<string, string>;
  /** Load the persisted avatar table from the backend (called on app start). */
  load: () => Promise<void>;
  /** Set (dataUrl) or clear (null) a custom avatar for an id — optimistic + persist. */
  setAvatar: (id: string, dataUrl: string | null) => Promise<void>;
}

/** Stable empty map so a "no avatars yet" load keeps a constant ref. */
const EMPTY: Record<string, string> = {};

export const useAvatars = create<AvatarsState>((set, get) => ({
  map: EMPTY,

  load: async () => {
    try {
      // Guard against a null/undefined response (e.g. a backend that returns nothing):
      // `map` must always be an object or `useAvatar` would index into null.
      set({ map: (await avatarsApi.get()) ?? EMPTY });
    } catch {
      // avatars are local-only UI personalization; a load failure is non-fatal.
    }
  },

  setAvatar: async (id, dataUrl) => {
    // Optimistic: update the local mirror immediately so every identity spot re-renders.
    set((s) => {
      const next = { ...s.map };
      if (dataUrl) next[id] = dataUrl;
      else delete next[id];
      return { map: next };
    });
    try {
      await avatarsApi.set(id, dataUrl);
    } catch {
      // On failure, reconcile from disk so the mirror matches what was actually stored.
      await get().load();
    }
  },
}));

/**
 * Subscribe to one identity's custom avatar by id. Returns the data-URL string when set,
 * else undefined (a stable primitive → a component only re-renders when this id's photo
 * actually changes; no black-screen-class unstable-ref risk).
 */
export function useAvatar(id: string | undefined): string | undefined {
  return useAvatars((s) => (id ? s.map?.[id] : undefined));
}
