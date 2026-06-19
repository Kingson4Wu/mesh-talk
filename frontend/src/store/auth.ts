import { create } from "zustand";
import { auth } from "@/lib/api";
import type { UserInfo } from "@/lib/types";

function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return "Something went wrong";
}

interface AuthState {
  user: UserInfo | null;
  loading: boolean;
  error: string | null;
  clearError: () => void;
  login: (username: string, password: string) => Promise<boolean>;
  register: (username: string, password: string) => Promise<boolean>;
  logout: () => Promise<void>;
}

export const useAuth = create<AuthState>((set) => ({
  user: null,
  loading: false,
  error: null,
  clearError: () => set({ error: null }),

  login: async (username, password) => {
    set({ loading: true, error: null });
    try {
      const res = await auth.login(username, password);
      if (!res.success || !res.user) throw new Error("Unable to sign in");
      set({ user: res.user, loading: false });
      return true;
    } catch (e) {
      set({ error: errMsg(e), loading: false });
      return false;
    }
  },

  register: async (username, password) => {
    set({ loading: true, error: null });
    try {
      const res = await auth.register(username, password);
      if (!res.success) throw new Error("Registration failed");
      set({ loading: false });
      return true;
    } catch (e) {
      set({ error: errMsg(e), loading: false });
      return false;
    }
  },

  logout: async () => {
    try {
      await auth.logout();
    } catch {
      // ignore — we clear the local session regardless
    }
    set({ user: null });
  },
}));
