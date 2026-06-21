import { useEffect, useState } from "react";
import { Bell, MinusSquare, Rocket, Settings } from "lucide-react";
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

export function SettingsDialog() {
  const [open, setOpen] = useState(false);
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

  // Persist the two non-autostart toggles together (the command takes both).
  const persist = (next: {
    minimize_to_tray: boolean;
    notifications: boolean;
  }) => {
    void settingsApi.set(next).catch(() => {});
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

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" title="Settings">
          <Settings className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Settings className="h-4 w-4" /> Settings
          </DialogTitle>
          <DialogDescription>
            Keep Mesh-Talk running in the background so it can receive messages
            and stay discoverable.
          </DialogDescription>
        </DialogHeader>

        <div className="grid gap-3">
          <ToggleRow
            id="setting-launch-at-login"
            icon={<Rocket className="h-4 w-4" />}
            title="Launch at login"
            desc="Start Mesh-Talk in the background when you sign in."
            checked={launchAtLogin}
            onChange={(v) => void onLaunch(v)}
            disabled={autostartBusy}
          />
          <ToggleRow
            id="setting-close-to-tray"
            icon={<MinusSquare className="h-4 w-4" />}
            title="Close to tray"
            desc="Closing the window hides it to the tray instead of quitting."
            checked={minimizeToTray}
            onChange={onMinimize}
          />
          <ToggleRow
            id="setting-notifications"
            icon={<Bell className="h-4 w-4" />}
            title="Notifications"
            desc="Notify me about new messages when the window isn't focused."
            checked={notifications}
            onChange={onNotifications}
          />
        </div>
      </DialogContent>
    </Dialog>
  );
}
