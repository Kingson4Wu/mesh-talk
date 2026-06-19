import { beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { useAppStore } from "../../src/stores/appStore.js";
import { useFeedbackStore } from "../../src/stores/feedbackStore.js";

const { mockInvoke } = vi.hoisted(() => ({ mockInvoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

// After the legacy stack was retired the app store holds only auth/session state;
// messaging/peers/channels live in the /redesign view (API.redesign), not here.
describe("appStore (auth)", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mockInvoke.mockReset();
    const feedback = useFeedbackStore();
    feedback.clearToasts();
    feedback.beginTask = vi.fn();
    feedback.endTask = vi.fn();
    feedback.showInfo = vi.fn();
    feedback.showSuccess = vi.fn();
    feedback.showError = vi.fn();
    feedback.clearLastError = vi.fn();
  });

  it("starts unauthenticated", () => {
    const store = useAppStore();
    expect(store.isAuthenticated).toBe(false);
    expect(store.user).toBeNull();
  });

  it("login sets the user and authenticates", async () => {
    const store = useAppStore();
    mockInvoke.mockResolvedValueOnce({
      success: true,
      user: { id: "u1", name: "Alice" },
    });

    const result = await store.login("alice", "password123");

    expect(result.success).toBe(true);
    expect(store.isAuthenticated).toBe(true);
    expect(store.user).toEqual({ id: "u1", name: "Alice" });
    expect(mockInvoke).toHaveBeenCalledWith("login", {
      username: "alice",
      password: "password123",
    });
  });

  it("login failure surfaces an error and stays unauthenticated", async () => {
    const store = useAppStore();
    mockInvoke.mockRejectedValueOnce(new Error("Invalid username or password"));

    const result = await store.login("alice", "wrong");

    expect(result.success).toBe(false);
    expect(store.isAuthenticated).toBe(false);
    expect(store.error).toContain("Unable to login");
  });

  it("logout clears the session", async () => {
    const store = useAppStore();
    mockInvoke.mockResolvedValueOnce({
      success: true,
      user: { id: "u1", name: "Alice" },
    });
    await store.login("alice", "password123");
    expect(store.isAuthenticated).toBe(true);

    mockInvoke.mockResolvedValueOnce({ success: true });
    await store.logout();

    expect(store.isAuthenticated).toBe(false);
    expect(store.user).toBeNull();
    expect(mockInvoke).toHaveBeenCalledWith("logout");
  });

  it("register forwards to the backend", async () => {
    const store = useAppStore();
    mockInvoke.mockResolvedValueOnce({
      success: true,
      user: { id: "u2", name: "Bob" },
    });

    const result = await store.register("bob", "password123");

    expect(result.success).toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("register", {
      username: "bob",
      password: "password123",
    });
  });
});
