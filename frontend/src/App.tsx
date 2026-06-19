import { LoginScreen } from "@/features/auth/LoginScreen";
import { Button } from "@/components/ui/button";
import { useAuth } from "@/store/auth";

export default function App() {
  const { user, logout } = useAuth();

  if (!user) return <LoginScreen />;

  // Placeholder until Phase 3 lands the chat UI.
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3">
      <p className="text-muted-foreground">
        Signed in as <span className="font-medium text-foreground">{user.username}</span>
      </p>
      <Button variant="outline" onClick={() => logout()}>
        Sign out
      </Button>
    </div>
  );
}
