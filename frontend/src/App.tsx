import { LoginScreen } from "@/features/auth/LoginScreen";
import { ChatApp } from "@/features/chat/ChatApp";
import { useAuth } from "@/store/auth";

export default function App() {
  const user = useAuth((s) => s.user);
  return user ? <ChatApp /> : <LoginScreen />;
}
