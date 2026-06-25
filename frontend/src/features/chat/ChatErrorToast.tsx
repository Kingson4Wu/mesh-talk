import { useTranslation } from "react-i18next";

/** The chat error banner, shared by the desktop + mobile shells (a transient bottom-centered toast). */
export function ChatErrorToast({
  error,
  clearError,
}: {
  error: string | null;
  clearError: () => void;
}) {
  const { t } = useTranslation();
  if (!error) return null;
  return (
    <div
      role="alert"
      className="absolute bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-3 rounded-md bg-destructive px-4 py-2 text-sm text-destructive-foreground shadow-lg"
    >
      <span>{error}</span>
      <button
        type="button"
        onClick={clearError}
        aria-label={t("conversation.dismissError")}
        className="font-bold opacity-80 hover:opacity-100"
      >
        ×
      </button>
    </div>
  );
}
