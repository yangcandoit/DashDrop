<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { initAppStore, destroyAppStore } from './store';
import NearbyView from './views/NearbyView.vue';
import TransfersView from './views/TransfersView.vue';
import HistoryView from './views/HistoryView.vue';
import TrustedDevicesView from './views/TrustedDevicesView.vue';
import SecurityEventsView from './views/SecurityEventsView.vue';
import SettingsView from './views/SettingsView.vue';

const currentView = ref('Nearby');
const showOnboarding = ref(false);

const navItems = [
  { id: 'Nearby', label: 'Nearby', icon: '📡' },
  { id: 'Transfers', label: 'Transfers', icon: '↗️' },
  { id: 'History', label: 'History', icon: '🕒' },
  { id: 'TrustedDevices', label: 'Trusted', icon: '🛡️' },
  { id: 'SecurityEvents', label: 'Security', icon: '🚨' },
  { id: 'Settings', label: 'Settings', icon: '⚙️' },
];

onMounted(() => {
  initAppStore();
  if (typeof window !== 'undefined') {
    if ((window as Window & { __DASHDROP_TEST_MOCK__?: unknown }).__DASHDROP_TEST_MOCK__) {
      showOnboarding.value = false;
      window.localStorage.setItem('dashdrop_onboarding_seen_v1', '1');
      return;
    }
    const seen = window.localStorage.getItem('dashdrop_onboarding_seen_v1');
    showOnboarding.value = seen !== '1';
  }
});

onUnmounted(() => {
  destroyAppStore();
});

const dismissOnboarding = () => {
  showOnboarding.value = false;
  if (typeof window !== 'undefined') {
    window.localStorage.setItem('dashdrop_onboarding_seen_v1', '1');
  }
};
</script>

<template>
  <div class="app-layout">
    <nav class="sidebar">
      <div class="logo-area">
        <svg viewBox="0 0 24 24" fill="none" class="icon-logo" stroke="url(#gradient)" stroke-width="2.5">
          <defs>
            <linearGradient id="gradient" x1="0%" y1="0%" x2="100%" y2="100%">
              <stop offset="0%" stop-color="#3b82f6" />
              <stop offset="100%" stop-color="#8b5cf6" />
            </linearGradient>
          </defs>
          <path stroke-linecap="round" stroke-linejoin="round" d="M13 10V3L4 14h7v7l9-11h-7z" />
        </svg>
        <h2 class="text-gradient" style="font-size: 1.4rem;">DashDrop</h2>
      </div>
      <ul class="nav-list">
        <li v-for="item in navItems" :key="item.id">
          <button 
            class="nav-btn" 
            :class="{ active: currentView === item.id }"
            @click="currentView = item.id"
          >
            <span class="nav-icon">{{ item.icon }}</span>
            <span class="nav-label">{{ item.label }}</span>
          </button>
        </li>
      </ul>
    </nav>
    <main class="main-content">
      <NearbyView v-if="currentView === 'Nearby'" @open-settings="currentView = 'Settings'" />
      <TransfersView v-if="currentView === 'Transfers'" @open-settings="currentView = 'Settings'" />
      <HistoryView v-if="currentView === 'History'" @open-settings="currentView = 'Settings'" />
      <TrustedDevicesView v-if="currentView === 'TrustedDevices'" @open-settings="currentView = 'Settings'" />
      <SecurityEventsView v-if="currentView === 'SecurityEvents'" @open-settings="currentView = 'Settings'" />
      <SettingsView v-if="currentView === 'Settings'" @back="currentView = 'Nearby'" />
    </main>
    <div v-if="showOnboarding" class="onboarding-backdrop">
      <section class="onboarding-card">
        <h3>Welcome to DashDrop</h3>
        <p class="text-muted">Before your first transfer:</p>
        <ul>
          <li>Verify device fingerprint before pairing unknown devices.</li>
          <li>Use Trusted Devices for auto-accept on known peers only.</li>
          <li>Keep both devices on the same LAN for best performance.</li>
        </ul>
        <button class="btn-primary" @click="dismissOnboarding">Got it</button>
      </section>
    </div>
  </div>
</template>

<style scoped>
.app-layout {
  display: flex;
  width: 100vw;
  height: 100vh;
  overflow: hidden;
}

.sidebar {
  width: 220px;
  background: var(--bg-surface-elevated);
  border-right: 1px solid var(--border-light);
  display: flex;
  flex-direction: column;
  padding: 24px 16px;
  z-index: 10;
}

.logo-area {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 40px;
  padding: 0 8px;
}

.icon-logo {
  width: 28px;
  height: 28px;
}

.nav-list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.nav-btn {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 14px;
  border-radius: var(--radius-md);
  background: transparent;
  border: 1px solid transparent;
  color: var(--text-secondary);
  font-size: 0.95rem;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
}

.nav-btn:hover {
  background: var(--bg-surface-hover);
  color: var(--text-primary);
}

.nav-btn.active {
  background: rgba(59, 130, 246, 0.1);
  color: #60a5fa;
  border-color: rgba(59, 130, 246, 0.2);
}

.nav-icon {
  font-size: 1.1rem;
}

.main-content {
  flex: 1;
  position: relative;
  overflow-y: hidden;
  background: var(--bg-app);
}

.onboarding-backdrop {
  position: fixed;
  inset: 0;
  z-index: 1000;
  background: rgba(4, 8, 17, 0.7);
  backdrop-filter: blur(6px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
}

.onboarding-card {
  width: min(560px, 100%);
  border-radius: var(--radius-xl);
  border: 1px solid var(--border-light);
  background: var(--bg-surface-elevated);
  padding: 22px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.onboarding-card h3 {
  margin: 0;
}

.onboarding-card ul {
  margin: 0 0 8px;
  padding-left: 20px;
}

.onboarding-card li {
  margin-bottom: 6px;
}
</style>
