import { useState } from "react";
import {
  Languages,
  LogOut,
  Palette,
  Settings as SettingsIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import {
  resolveLanguage,
  setLanguage,
  SUPPORTED_LANGUAGES,
  type Language,
} from "@/lib/i18n";
import { ThemePicker } from "@/features/chat/ThemePicker";
import { useAuth } from "@/store/auth";

const LANGUAGE_LABELS: Record<Language, string> = {
  en: "English",
  es: "Español",
  ja: "日本語",
  "zh-Hans": "简体中文",
  "zh-Hant": "繁體中文",
  yue: "粵語",
};

/** Mobile settings: ONLY what makes sense on a phone — appearance, language, sign out. No relay
 * hosting, native file folders, window controls, or gateway-relay config (those are desktop-only;
 * the phone's relay is set from the scanned invite). */
export function MobileSettings() {
  const { t, i18n } = useTranslation();
  const [open, setOpen] = useState(false);
  const logout = useAuth((s) => s.logout);
  const currentLang = resolveLanguage(i18n.language) ?? "en";

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="mobile-settings"
          title={t("settings.title")}
          aria-label={t("settings.title")}
        >
          <SettingsIcon className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent data-testid="settings-dialog">
        <DialogHeader>
          <DialogTitle>{t("settings.title")}</DialogTitle>
        </DialogHeader>
        <div className="space-y-5">
          <div>
            <div className="flex items-center gap-2 text-sm font-medium">
              <Palette className="h-4 w-4 text-muted-foreground" />
              {t("settings.theme")}
            </div>
            <p className="mb-3 mt-0.5 text-xs text-muted-foreground">
              {t("settings.themeDesc")}
            </p>
            <ThemePicker />
          </div>
          <div className="flex items-center justify-between gap-4">
            <div className="flex items-center gap-2 text-sm font-medium">
              <Languages className="h-4 w-4 text-muted-foreground" />
              {t("settings.language")}
            </div>
            <select
              data-testid="settings-language-select"
              value={currentLang}
              onChange={(e) => setLanguage(e.target.value as Language)}
              aria-label={t("settings.language")}
              className="h-9 rounded-md border border-input bg-background px-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              {SUPPORTED_LANGUAGES.map((lng) => (
                <option key={lng} value={lng}>
                  {LANGUAGE_LABELS[lng]}
                </option>
              ))}
            </select>
          </div>
          <button
            type="button"
            data-testid="sidebar-sign-out"
            onClick={() => logout()}
            className="flex w-full items-center gap-2.5 rounded-md px-2 py-2 text-left text-sm text-destructive transition-colors hover:bg-destructive/10"
          >
            <LogOut className="h-4 w-4" />
            <span>{t("sidebar.signOut")}</span>
          </button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
