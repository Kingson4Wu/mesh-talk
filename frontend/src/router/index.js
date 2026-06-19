import { createRouter, createWebHashHistory } from "vue-router";
import LoginView from "../views/auth/LoginView.vue";
import RedesignChatView from "../views/redesign/RedesignChatView.vue";

// The redesign chat is the whole app now; it lives at "/".
const routes = [
  {
    path: "/",
    name: "redesign",
    component: RedesignChatView,
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
