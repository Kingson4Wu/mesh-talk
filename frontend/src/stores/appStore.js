import { defineStore } from "pinia";
import { computed, ref } from "vue";
import { API } from "../services/api";
import { useFeedbackStore } from "./feedbackStore";
import Logger from "../utils/logger";

// App-level store. After the legacy chat stack was retired this holds only auth/session
// state; the chat view owns its own messaging/peer/channel state (it talks to the
// node directly via API.chat and listens for messaging Tauri events).
export const useAppStore = defineStore("app", () => {
  const feedback = useFeedbackStore();

  const user = ref(null);
  const loading = ref(false);
  const error = ref(null);

  const isAuthenticated = computed(() => Boolean(user.value));

  function setLoading(value) {
    loading.value = value;
  }

  function setError(err, options = {}) {
    if (!err) {
      error.value = null;
      if (options.clearLastError) {
        feedback.clearLastError();
      }
      return;
    }
    Logger.error(err);
    const message =
      options.message ||
      (typeof err === "string" ? err : (err?.message ?? "Unknown error"));
    error.value = message;
    feedback.showError(err, {
      message,
      title: options.title,
      toast: options.toast !== false,
      context: options.source ?? "appStore",
    });
  }

  async function login(username, password) {
    setLoading(true);
    setError(null, { clearLastError: true });
    const taskKey = feedback.beginTask("auth:login", "Signing in…");
    try {
      const result = await API.auth.login(username, password);
      if (!result.success) {
        throw new Error(result.error ?? "Unable to login");
      }
      user.value = result.user;
      feedback.showSuccess(`Signed in as ${result.user?.name ?? username}`, {
        autoDismiss: 3200,
      });
      return { success: true };
    } catch (err) {
      setError(err, { message: "Unable to login", source: "auth.login" });
      return { success: false, error: error.value };
    } finally {
      setLoading(false);
      feedback.endTask(taskKey);
    }
  }

  async function register(username, password) {
    setLoading(true);
    setError(null, { clearLastError: true });
    const taskKey = feedback.beginTask("auth:register", "Creating account…");
    try {
      const result = await API.auth.register(username, password);
      if (!result.success) {
        throw new Error(result.error ?? "Registration failed");
      }
      feedback.showSuccess("Account created", {
        detail: `Sign in as ${result.user?.name ?? username}`,
      });
      return { success: true, user: result.user };
    } catch (err) {
      setError(err, { message: "Registration failed", source: "auth.register" });
      return { success: false, error: error.value };
    } finally {
      setLoading(false);
      feedback.endTask(taskKey);
    }
  }

  async function logout() {
    setLoading(true);
    setError(null, { clearLastError: true });
    const taskKey = feedback.beginTask("auth:logout", "Signing out…");
    try {
      const result = await API.auth.logout();
      if (!result.success) {
        throw new Error(result.error ?? "Logout failed");
      }
      feedback.showInfo("Signed out", { autoDismiss: 2500 });
    } catch (err) {
      setError(err, { message: "Logout failed", source: "auth.logout" });
    } finally {
      user.value = null;
      setLoading(false);
      feedback.endTask(taskKey);
    }
  }

  return {
    user,
    loading,
    error,
    isAuthenticated,
    setError,
    login,
    register,
    logout,
  };
});
