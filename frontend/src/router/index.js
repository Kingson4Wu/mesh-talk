import { createRouter, createWebHashHistory } from "vue-router";
import LoginView from "../views/auth/LoginView.vue";
import ChatView from "../views/chat/ChatView.vue";

// The chat is the whole app now; it lives at "/".
const routes = [
  {
    path: "/",
    name: "chat",
    component: ChatView,
    meta: { requiresAuth: true },
  },
  {
    path: "/login",
    name: "login",
    component: LoginView,
  },
  {
    path: "/:pathMatch(.*)*",
    redirect: "/",
  },
];

const router = createRouter({
  history: createWebHashHistory(),
  routes,
});

export default router;
