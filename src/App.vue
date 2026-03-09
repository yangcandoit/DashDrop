<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { initAppStore, destroyAppStore, systemError } from './store';
import NearbyView from './views/NearbyView.vue';
import TransfersView from './views/TransfersView.vue';
import HistoryView from './views/HistoryView.vue';
import TrustedDevicesView from './views/TrustedDevicesView.vue';
import SecurityEventsView from './views/SecurityEventsView.vue';
import SettingsView from './views/SettingsView.vue';
import SystemNotice from './components/SystemNotice.vue';

const currentView = ref('Nearby');
const showOnboarding = ref(false);

const navItems = [
  { id: 'Nearby', label: 'Nearby' },
  { id: 'Transfers', label: 'Transfers' },
  { id: 'History', label: 'History' },
  { id: 'TrustedDevices', label: 'Trusted Devices' },
  { id: 'SecurityEvents', label: 'Security Events' },
  { id: 'Settings', label: 'Settings' },
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

const dismissSystemError = () => {
  systemError.value = null;
};
</script>

<template>
  <div class="app-shell">
    <nav class="app-rail">
      <div class="brand-block">
        <div class="brand-title">DashDrop</div>
      </div>
      <ul class="rail-nav">
        <li v-for="item in navItems" :key="item.id">
          <button
            class="rail-btn"
            :class="{ active: currentView === item.id }"
            @click="currentView = item.id"
          >
            {{ item.label }}
          </button>
        </li>
      </ul>
    </nav>

    <main class="app-workspace">
      <SystemNotice
        v-if="systemError"
        :message="systemError"
        @dismiss="dismissSystemError"
      />
      <div class="workspace-body">
        <NearbyView v-if="currentView === 'Nearby'" @open-settings="currentView = 'Settings'" />
        <TransfersView v-if="currentView === 'Transfers'" @open-settings="currentView = 'Settings'" />
        <HistoryView v-if="currentView === 'History'" @open-settings="currentView = 'Settings'" />
        <TrustedDevicesView v-if="currentView === 'TrustedDevices'" @open-settings="currentView = 'Settings'" />
        <SecurityEventsView v-if="currentView === 'SecurityEvents'" @open-settings="currentView = 'Settings'" />
        <SettingsView v-if="currentView === 'Settings'" @back="currentView = 'Nearby'" />
      </div>
    </main>

    <div v-if="showOnboarding" class="onboarding-backdrop">
      <section class="onboarding-card">
        <h3>Before You Start</h3>
        <p class="text-muted">Verify fingerprint before sending to a new device.</p>
        <button class="btn btn-primary" @click="dismissOnboarding">Continue</button>
      </section>
    </div>
  </div>
</template>

<style scoped>
.app-shell {
  display: flex;
  gap: 12px;
  padding: 12px;
  width: 100vw;
  height: 100vh;
  box-sizing: border-box;
}

.app-rail {
  width: 200px;
  border-radius: 16px;
  background: var(--surface);
  border: 1px solid var(--border-subtle);
  display: flex;
  flex-direction: column;
  padding: 14px;
}

.brand-block {
  margin-bottom: 10px;
  padding-bottom: 10px;
  border-bottom: 1px solid var(--border-subtle);
}

.brand-title {
  font-size: 1.32rem;
  font-weight: 600;
  color: var(--text-primary);
}

.rail-nav {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.rail-btn {
  width: 100%;
  text-align: left;
  padding: 8px 10px;
  border-radius: 10px;
  border: 1px solid transparent;
  background: transparent;
  color: var(--text-secondary);
  font-size: 0.92rem;
  font-weight: 500;
  cursor: pointer;
}

.rail-btn:hover {
  background: var(--surface-muted);
}

.rail-btn.active {
  background: color-mix(in srgb, var(--accent) 10%, #fff);
  border-color: color-mix(in srgb, var(--accent) 35%, transparent);
  color: #005bb5;
}

.app-workspace {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  border-radius: 16px;
  border: 1px solid var(--border-subtle);
  background: var(--surface);
}

.workspace-body {
  flex: 1;
  min-height: 0;
}

.onboarding-backdrop {
  position: fixed;
  inset: 0;
  z-index: 1000;
  background: rgba(0, 0, 0, 0.2);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
}

.onboarding-card {
  width: min(420px, 100%);
  border-radius: 14px;
  border: 1px solid var(--border-subtle);
  background: var(--surface);
  box-shadow: var(--shadow-soft);
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

@media (max-width: 900px) {
  .app-shell {
    padding: 8px;
    gap: 8px;
  }

  .app-rail {
    width: 170px;
    padding: 10px;
  }

  .brand-title {
    font-size: 1.2rem;
  }
}
</style>
