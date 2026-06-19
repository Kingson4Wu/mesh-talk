import { useState } from "react";
import { MessagesSquare, Loader2, ShieldCheck } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { useAuth } from "@/store/auth";

export function LoginScreen() {
  const { login, register, loading, error, clearError } = useAuth();
  const [tab, setTab] = useState<"signin" | "register">("signin");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [notice, setNotice] = useState<string | null>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setNotice(null);
    if (tab === "signin") {
      await login(username.trim(), password);
    } else {
      const ok = await register(username.trim(), password);
      if (ok) {
        setNotice("Account created — you can sign in now.");
        setTab("signin");
        setPassword("");
      }
    }
  };

  const onTab = (v: string) => {
    setTab(v as "signin" | "register");
    clearError();
    setNotice(null);
  };

  return (
    <div className="relative flex h-full items-center justify-center overflow-hidden p-6">
      {/* ambient glow */}
      <div className="pointer-events-none absolute -top-32 left-1/2 h-96 w-96 -translate-x-1/2 rounded-full bg-primary/20 blur-3xl" />
      <div className="pointer-events-none absolute bottom-0 right-0 h-80 w-80 rounded-full bg-primary/10 blur-3xl" />

      <div className="relative w-full max-w-sm">
        <div className="mb-8 flex flex-col items-center text-center">
          <div className="mb-4 flex h-14 w-14 items-center justify-center rounded-2xl bg-primary text-primary-foreground shadow-lg shadow-primary/30">
            <MessagesSquare className="h-7 w-7" />
          </div>
          <h1 className="text-2xl font-semibold tracking-tight">Mesh-Talk</h1>
          <p className="mt-1 flex items-center gap-1.5 text-sm text-muted-foreground">
            <ShieldCheck className="h-3.5 w-3.5" />
            Serverless · end-to-end encrypted
          </p>
        </div>

        <div className="rounded-2xl border bg-card/60 p-6 shadow-xl backdrop-blur">
          <Tabs value={tab} onValueChange={onTab}>
            <TabsList className="grid w-full grid-cols-2">
              <TabsTrigger value="signin">Sign in</TabsTrigger>
              <TabsTrigger value="register">Register</TabsTrigger>
            </TabsList>

            <TabsContent value={tab} forceMount>
              <form onSubmit={submit} className="mt-5 space-y-4">
                <div className="space-y-2">
                  <Label htmlFor="username">Username</Label>
                  <Input
                    id="username"
                    autoFocus
                    autoComplete="username"
                    value={username}
                    onChange={(e) => setUsername(e.target.value)}
                    placeholder="alice"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="password">Password</Label>
                  <Input
                    id="password"
                    type="password"
                    autoComplete={
                      tab === "signin" ? "current-password" : "new-password"
                    }
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    placeholder={
                      tab === "register" ? "at least 8 characters" : "••••••••"
                    }
                  />
                </div>

                {error && (
                  <p className="text-sm font-medium text-destructive">{error}</p>
                )}
                {notice && (
                  <p className="text-sm font-medium text-primary">{notice}</p>
                )}

                <Button
                  type="submit"
                  className="w-full"
                  disabled={loading || !username.trim() || !password}
                >
                  {loading && <Loader2 className="h-4 w-4 animate-spin" />}
                  {tab === "signin" ? "Sign in" : "Create account"}
                </Button>
              </form>
            </TabsContent>
          </Tabs>
        </div>

        <p className="mt-6 text-center text-xs text-muted-foreground">
          Your keys never leave this device. Peers are discovered on your LAN.
        </p>
      </div>
    </div>
  );
}
