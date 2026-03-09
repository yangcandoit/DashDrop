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
        <p class="brand-subtitle">Local transfer desk</p>
      </div>
      <ul class="rail-nav">
        <li v-for="(item, index) in navItems" :key="item.id">
          <button 
            class="rail-btn" 
            :class="{ active: currentView === item.id }"
            @click="currentView = item.id"
          >
            <span class="rail-index">{{ String(index + 1).padStart(2, '0') }}</span>
            <span>{{ item.label }}</span>
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
        <p class="text-muted">Use this checklist for safer first-time transfers:</p>
        <ul>
          <li>Verify fingerprint when pairing unfamiliar devices.</li>
          <li>Enable auto-accept only for trusted devices.</li>
          <li>Keep both devices on the same local network.</li>
        </ul>
        <button class="btn btn-primary" @click="dismissOnboarding">Continue</button>
      </section>
    </div>
  </div>
</template>

<style scoped>
.app-shell {
  display: flex;
  gap: 18px;
  padding: 18px;
  width: 100vw;
  height: 100vh;
  box-sizing: border-box;
}

.app-rail {
  width: 250px;
  border-radius: 24px;
  background: var(--surface-strong);
  border: 1px solid var(--border-subtle);
  box-shadow: var(--shadow-soft);
  display: flex;
  flex-direction: column;
  padding: 24px 18px;
  overflow: hidden;
}

.brand-block {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 28px;
  padding: 6px 8px 12px;
  border-bottom: 1px solid var(--border-subtle);
}

.brand-title {
  width: 100%;
  font-family: var(--font-display);
  font-size: 1.55rem;
  letter-spacing: -0.01em;
  color: var(--text-primary);
}

.brand-subtitle {
  width: 100%;
  margin: 0;
  color: var(--text-muted);
  font-size: 0.78rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.rail-nav {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.rail-btn {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 12px;
  border-radius: 14px;
  background: transparent;
  border: 1px solid var(--border-subtle);
  color: var(--text-muted);
  font-size: 0.9rem;
  font-weight: 600;
  letter-spacing: 0.02em;
  cursor: pointer;
  transition: transform 220ms ease, border-color 220ms ease, background-color 220ms ease, color 220ms ease;
}

.rail-btn:hover {
  transform: translateX(2px);
  color: var(--text-primary);
  border-color: var(--border-strong);
  background: rgba(255, 255, 255, 0.45);
}

.rail-btn.active {
  color: var(--text-primary);
  border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  background: linear-gradient(120deg, color-mix(in srgb, var(--accent) 22%, #fff), rgba(255, 255, 255, 0.92));
}

.rail-index {
  min-width: 24px;
  font-size: 0.72rem;
  font-weight: 700;
  color: var(--text-subtle);
}

.app-workspace {
  flex: 1;
  position: relative;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  border-radius: 24px;
  border: 1px solid var(--border-subtle);
  background: var(--surface);
  box-shadow: var(--shadow-soft);
}

.workspace-body {
  flex: 1;
  min-height: 0;
}

.onboarding-backdrop {
  position: fixed;
  inset: 0;
  z-index: 1000;
  background: rgba(33, 30, 24, 0.48);
  backdrop-filter: blur(8px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
}

.onboarding-card {
  width: min(560px, 100%);
  border-radius: 20px;
  border: 1px solid var(--border-subtle);
  background: #fffcf6;
  box-shadow: 0 30px 70px rgba(33, 29, 22, 0.22);
  padding: 24px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.onboarding-card h3 {
  margin: 0;
  font-family: var(--font-display);
  font-size: 1.5rem;
}

.onboarding-card ul {
  margin: 0 0 8px;
  padding-left: 18px;
  color: var(--text-secondary);
}

.onboarding-card li {
  margin-bottom: 8px;
}

@media (max-width: 900px) {
  .app-shell {
    padding: 10px;
    gap: 10px;
  }

  .app-rail {
    width: 180px;
    padding: 14px 10px;
  }

  .brand-title {
    font-size: 1.2rem;
  }

  .brand-subtitle {
    font-size: 0.65rem;
  }

  .rail-btn {
    padding: 10px 8px;
    font-size: 0.78rem;
  }

  .rail-index {
    display: none;
  }
}
</style>
