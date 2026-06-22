import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  Bell,
  FolderOpen,
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

/** A section group: a small display-font label over a stack of rows. */
function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-2">
      <h3 className="font-mono text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
        {title}
      </h3>
      <div className="grid gap-2">{children}</div>
    </section>
  );
}

/** A row scaffold: icon + label/help on the left, a control on the right. */
function Row({
  id,
  icon,
  title,
  desc,
  control,
}: {
  id?: string;
  icon: React.ReactNode;
  title: string;
  desc: string;
  control: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 rounded-lg border p-3">
      <div className="flex min-w-0 items-start gap-3">
        <div className="mt-0.5 text-muted-foreground">{icon}</div>
        <div className="min-w-0">
          <label htmlFor={id} className="block text-sm font-medium">
            {title}
          </label>
          <p className="text-xs leading-relaxed text-muted-foreground">
            {desc}
          </p>
        </div>
      </div>
      <div className="shrink-0">{control}</div>
    </div>
  );
}

const SELECT_CLASS =
  "h-9 rounded-md border border-input bg-background px-2 text-sm transition-colors hover:border-ring focus-visible:border-ring focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";

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
  const [downloadDir, setDownloadDir] = useState("");

  // Load current state whenever the dialog opens.
  useEffect(() => {
    if (!open) return;
    void settingsApi.get().then(
      (s) => {
        setMinimizeToTray(s.minimize_to_tray);
        setNotifications(s.notifications);
        setDownloadDir(s.download_dir);
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

  const chooseDir = async () => {
    try {
      const dir = await openDialog({
        directory: true,
        defaultPath: downloadDir || undefined,
      });
      if (typeof dir === "string") {
        const cur = await settingsApi.get();
        await settingsApi.set({ ...cur, download_dir: dir });
        setDownloadDir(dir);
      }
    } catch {
      // Cancelled / unavailable — leave the current folder untouched.
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
        <Button
          variant="ghost"
          size="icon"
          data-testid="sidebar-action-settings"
          title={t("settings.title")}
        >
          <Settings className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-md" data-testid="settings-dialog">
        <DialogHeader>
          <DialogTitle>{t("settings.title")}</DialogTitle>
          <DialogDescription>{t("settings.description")}</DialogDescription>
        </DialogHeader>

        <div className="grid max-h-[70vh] gap-5 overflow-y-auto pr-0.5">
          <Section title={t("settings.sectionAppearance")}>
            <Row
              id="setting-theme"
              icon={<Palette className="h-4 w-4" />}
              title={t("settings.theme")}
              desc={t("settings.themeDesc")}
              control={
                <select
                  id="setting-theme"
                  data-testid="settings-theme-select"
                  value={theme}
                  onChange={(e) => setTheme(e.target.value as Theme)}
                  className={SELECT_CLASS}
                  aria-label={t("settings.theme")}
                >
                  {THEMES.map((th) => (
                    <option key={th} value={th}>
                      {t(`settings.theme_${th}`)}
                    </option>
                  ))}
                </select>
              }
            />
            <Row
              id="setting-language"
              icon={<Languages className="h-4 w-4" />}
              title={t("settings.language")}
              desc={t("settings.languageDesc")}
              control={
                <select
                  id="setting-language"
                  data-testid="settings-language-select"
                  value={currentLang}
                  onChange={(e) => setLanguage(e.target.value as Language)}
                  className={SELECT_CLASS}
                  aria-label={t("settings.language")}
                >
                  {SUPPORTED_LANGUAGES.map((lng) => (
                    <option key={lng} value={lng}>
                      {LANGUAGE_LABELS[lng]}
                    </option>
                  ))}
                </select>
              }
            />
          </Section>

          <Section title={t("settings.sectionBackground")}>
            <Row
              id="setting-launch-at-login"
              icon={<Rocket className="h-4 w-4" />}
              title={t("settings.launchAtLogin")}
              desc={t("settings.launchAtLoginDesc")}
              control={
                <Switch
                  id="setting-launch-at-login"
                  checked={launchAtLogin}
                  onCheckedChange={(v) => void onLaunch(v)}
                  disabled={autostartBusy}
                  aria-label={t("settings.launchAtLogin")}
                />
              }
            />
            <Row
              id="setting-close-to-tray"
              icon={<MinusSquare className="h-4 w-4" />}
              title={t("settings.closeToTray")}
              desc={t("settings.closeToTrayDesc")}
              control={
                <Switch
                  id="setting-close-to-tray"
                  checked={minimizeToTray}
                  onCheckedChange={onMinimize}
                  aria-label={t("settings.closeToTray")}
                />
              }
            />
            <Row
              id="setting-notifications"
              icon={<Bell className="h-4 w-4" />}
              title={t("settings.notifications")}
              desc={t("settings.notificationsDesc")}
              control={
                <Switch
                  id="setting-notifications"
                  checked={notifications}
                  onCheckedChange={onNotifications}
                  aria-label={t("settings.notifications")}
                />
              }
            />
          </Section>

          <Section title={t("settings.sectionFiles")}>
            <Row
              icon={<FolderOpen className="h-4 w-4" />}
              title={t("settings.downloadFolder")}
              desc={
                downloadDir
                  ? t("settings.downloadFolderSet")
                  : t("settings.downloadFolderDesc")
              }
              control={
                <div className="flex max-w-[11rem] flex-col items-end gap-1">
                  {downloadDir && (
                    <span
                      className="max-w-full truncate font-mono text-[11px] text-muted-foreground"
                      title={downloadDir}
                    >
                      {downloadDir}
                    </span>
                  )}
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => void chooseDir()}
                  >
                    {downloadDir
                      ? t("settings.changeFolder")
                      : t("settings.chooseFolder")}
                  </Button>
                </div>
              }
            />
          </Section>
        </div>
      </DialogContent>
    </Dialog>
  );
}
