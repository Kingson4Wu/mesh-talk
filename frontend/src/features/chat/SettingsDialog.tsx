import { useEffect, useState } from "react";
import {
  Bell,
  Languages,
  MinusSquare,
  Palette,
  Rocket,
  Settings,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { settings as settingsApi } from "@/lib/api";
import { setLanguage, SUPPORTED_LANGUAGES, type Language } from "@/lib/i18n";
import { useTheme, type Theme } from "@/lib/theme";

const THEMES: Theme[] = ["light", "dark", "oled"];

interface Row {
  id: string;
  icon: React.ReactNode;
  title: string;
  desc: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}

function ToggleRow({
  id,
  icon,
  title,
  desc,
  checked,
  onChange,
  disabled,
}: Row) {
  return (
    <div className="flex items-center justify-between gap-4 rounded-lg border p-3">
      <div className="flex min-w-0 items-start gap-3">
        <div className="mt-0.5 text-muted-foreground">{icon}</div>
        <div className="min-w-0">
          <label htmlFor={id} className="block text-sm font-medium">
            {title}
          </label>
          <p className="text-xs text-muted-foreground">{desc}</p>
        </div>
      </div>
      <Switch
        id={id}
        checked={checked}
        onCheckedChange={onChange}
        disabled={disabled}
        aria-label={title}
      />
    </div>
  );
}

const LANGUAGE_LABELS: Record<Language, string> = {
  en: "English",
  zh: "中文",
};

export function SettingsDialog() {
  const { t, i18n } = useTranslation();
  const [open, setOpen] = useState(false);
  // Selectors return primitives only (stable refs) — a fresh object once black-screened the app.
  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.set);
  // Primitive state only (memory: zustand selectors returning fresh objects each
  // render once black-screened the app — kept as local primitives here regardless).
  const [minimizeToTray, setMinimizeToTray] = useState(true);
  const [notifications, setNotifications] = useState(true);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [autostartBusy, setAutostartBusy] = useState(false);

  // Load current state whenever the dialog opens.
  useEffect(() => {
    if (!open) return;
    void settingsApi.get().then(
      (s) => {
        setMinimizeToTray(s.minimize_to_tray);
        setNotifications(s.notifications);
      },
      () => {},
    );
    void settingsApi.autostartEnabled().then(
      (on) => setLaunchAtLogin(on),
      () => {},
    );
  }, [open]);

  // Persist the toggles. Re-read the current settings first so we don't clobber fields the
  // dialog doesn't manage (e.g. download_dir, set in the Files tray).
  const persist = (next: {
    minimize_to_tray: boolean;
    notifications: boolean;
  }) => {
    void settingsApi
      .get()
      .then((cur) => settingsApi.set({ ...cur, ...next }))
      .catch(() => {});
  };

  const onMinimize = (v: boolean) => {
    setMinimizeToTray(v);
    persist({ minimize_to_tray: v, notifications });
  };
  const onNotifications = (v: boolean) => {
    setNotifications(v);
    persist({ minimize_to_tray: minimizeToTray, notifications: v });
  };
  const onLaunch = async (v: boolean) => {
    setAutostartBusy(true);
    try {
      await settingsApi.setAutostart(v);
      // Reflect the actual OS state rather than the optimistic toggle.
      setLaunchAtLogin(await settingsApi.autostartEnabled());
    } catch {
      // Revert on failure.
      setLaunchAtLogin(await settingsApi.autostartEnabled().catch(() => !v));
    } finally {
      setAutostartBusy(false);
    }
  };

  // The active base language (i18n.language may be a region code like "en-US").
  const currentLang = (
    SUPPORTED_LANGUAGES.includes(i18n.language as Language)
      ? i18n.language
      : i18n.language.split("-")[0]
  ) as Language;

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" title={t("settings.title")}>
          <Settings className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Settings className="h-4 w-4" /> {t("settings.title")}
          </DialogTitle>
          <DialogDescription>{t("settings.description")}</DialogDescription>
        </DialogHeader>

        <div className="grid gap-3">
          <div className="flex items-center justify-between gap-4 rounded-lg border p-3">
            <div className="flex min-w-0 items-start gap-3">
              <div className="mt-0.5 text-muted-foreground">
                <Languages className="h-4 w-4" />
              </div>
              <div className="min-w-0">
                <label
                  htmlFor="setting-language"
                  className="block text-sm font-medium"
                >
                  {t("settings.language")}
                </label>
                <p className="text-xs text-muted-foreground">
                  {t("settings.languageDesc")}
                </p>
              </div>
            </div>
            <select
              id="setting-language"
              value={currentLang}
              onChange={(e) => setLanguage(e.target.value as Language)}
              className="h-9 rounded-md border border-input bg-background px-2 text-sm"
              aria-label={t("settings.language")}
            >
              {SUPPORTED_LANGUAGES.map((lng) => (
                <option key={lng} value={lng}>
                  {LANGUAGE_LABELS[lng]}
                </option>
              ))}
            </select>
          </div>
          <div className="flex items-center justify-between gap-4 rounded-lg border p-3">
            <div className="flex min-w-0 items-start gap-3">
              <div className="mt-0.5 text-muted-foreground">
                <Palette className="h-4 w-4" />
              </div>
              <div className="min-w-0">
                <label
                  htmlFor="setting-theme"
                  className="block text-sm font-medium"
                >
                  {t("settings.theme")}
                </label>
                <p className="text-xs text-muted-foreground">
                  {t("settings.themeDesc")}
                </p>
              </div>
            </div>
            <select
              id="setting-theme"
              value={theme}
              onChange={(e) => setTheme(e.target.value as Theme)}
              className="h-9 rounded-md border border-input bg-background px-2 text-sm"
              aria-label={t("settings.theme")}
            >
              {THEMES.map((th) => (
                <option key={th} value={th}>
                  {t(`settings.theme_${th}`)}
                </option>
              ))}
            </select>
          </div>
          <ToggleRow
            id="setting-launch-at-login"
            icon={<Rocket className="h-4 w-4" />}
            title={t("settings.launchAtLogin")}
            desc={t("settings.launchAtLoginDesc")}
            checked={launchAtLogin}
            onChange={(v) => void onLaunch(v)}
            disabled={autostartBusy}
          />
          <ToggleRow
            id="setting-close-to-tray"
            icon={<MinusSquare className="h-4 w-4" />}
            title={t("settings.closeToTray")}
            desc={t("settings.closeToTrayDesc")}
            checked={minimizeToTray}
            onChange={onMinimize}
          />
          <ToggleRow
            id="setting-notifications"
            icon={<Bell className="h-4 w-4" />}
            title={t("settings.notifications")}
            desc={t("settings.notificationsDesc")}
            checked={notifications}
            onChange={onNotifications}
          />
        </div>
      </DialogContent>
    </Dialog>
  );
}
