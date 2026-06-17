<template>
  <div class="redesign">
    <header class="rd-header">
      <h2>Redesign chat <span class="beta">beta</span></h2>
      <div class="me">you: <code>{{ myId || "(starting…)" }}</code></div>
      <router-link class="back" :to="{ name: 'chat' }">← Back</router-link>
    </header>
    <p v-if="error" class="error">{{ error }}</p>
    <p>Peers discovered: {{ peers.length }}</p>
  </div>
</template>

<script setup>
import { ref, onMounted } from "vue";
import { useRouter } from "vue-router";
import { useAppStore } from "../../stores/appStore";
import { API } from "../../services/api";

const router = useRouter();
const store = useAppStore();
const myId = ref("");
const peers = ref([]);
const error = ref("");

onMounted(async () => {
  if (!store.isAuthenticated) {
    router.replace({ name: "login" });
    return;
  }
  try {
    myId.value = await API.redesign.myId();
  } catch (e) {
    error.value = "Redesign node not started yet.";
  }
  try {
    peers.value = await API.redesign.listPeers();
  } catch (e) {
    /* node may still be starting */
  }
});
</script>

<style scoped>
.redesign {
  height: 100vh;
  padding: 16px;
  color: rgba(226, 232, 240, 1);
  background: rgba(15, 23, 42, 1);
}
.rd-header {
  display: flex;
  align-items: center;
  gap: 16px;
}
.beta {
  font-size: 11px;
  color: #4ade80;
  border: 1px solid #4ade80;
  border-radius: 6px;
  padding: 1px 6px;
}
.me code {
  color: rgba(148, 163, 184, 1);
}
.back {
  margin-left: auto;
  color: rgba(59, 130, 246, 1);
  text-decoration: none;
}
.error {
  color: #f87171;
}
</style>
