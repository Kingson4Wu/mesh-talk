import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  ExternalLink,
  Github,
  Info,
  MessagesSquare,
  Scale,
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
import { obs } from "@/lib/api";

const REPO_URL = "https://github.com/Kingson4Wu/mesh-talk";
const LICENSE_URL = "https://github.com/Kingson4Wu/mesh-talk/blob/main/LICENSE";

/** A minimal About surface: app name + version, one-line description, source/license
 * links, and a shortcut into the Diagnostics dialog. Version comes from `env_info`. */
export function AboutDialog() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [version, setVersion] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    void obs.envInfo().then(
      (e) => setVersion(e.app_version),
      () => setVersion(null),
    );
  }, [open]);

  const open_ = (url: string) => {
    void openUrl(url).catch(() => {});
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" title={t("about.title")}>
          <Info className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Info className="h-4 w-4" /> {t("about.title")}
          </DialogTitle>
          <DialogDescription>{t("about.description")}</DialogDescription>
        </DialogHeader>

        <div className="grid gap-4">
          <div className="flex items-center gap-3">
            <div className="flex h-12 w-12 items-center justify-center rounded-2xl bg-primary text-primary-foreground">
              <MessagesSquare className="h-6 w-6" />
            </div>
            <div className="min-w-0">
              <div className="text-base font-semibold">Mesh-Talk</div>
              <div className="font-mono text-xs text-muted-foreground">
                {version
                  ? t("about.version", { version })
                  : t("diagnostics.nodeNotReady")}
              </div>
            </div>
          </div>

          <p className="text-sm text-muted-foreground">{t("about.tagline")}</p>

          <div className="grid gap-2">
            <Button
              variant="secondary"
              size="sm"
              className="justify-start"
              onClick={() => open_(REPO_URL)}
            >
              <Github className="h-4 w-4" /> {t("about.sourceRepo")}
              <ExternalLink className="ml-auto h-3.5 w-3.5 opacity-60" />
            </Button>
            <Button
              variant="secondary"
              size="sm"
              className="justify-start"
              onClick={() => open_(LICENSE_URL)}
            >
              <Scale className="h-4 w-4" /> {t("about.license")}
              <ExternalLink className="ml-auto h-3.5 w-3.5 opacity-60" />
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
