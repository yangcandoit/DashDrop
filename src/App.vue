<script setup lang="ts">
import { ref, onMounted, onUnmounted, watch } from 'vue';
import {
  initAppStore,
  destroyAppStore,
  systemError,
  externalSharePaths,
  pendingPairingLink,
  pendingNavigationTarget,
  clearPendingNavigationRequest,
  type SystemNoticeTarget,
} from './store';
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

onMounted(async () => {
  try {
    await initAppStore();
  } catch (e) {
    console.error('Failed to initialize app store', e);
    systemError.value = {
      message:
        'Failed to initialize DashDrop. Next: Relaunch the app. If this keeps happening, open Settings diagnostics after restart and check backend/runtime status.',
      tone: 'error',
      code: 'APP_INIT_FAILED',
      actionLabel: 'Open Settings',
      actionTarget: 'Settings',
    };
  }
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

watch(
  externalSharePaths,
  (paths) => {
    // External-share handoff always routes to Nearby first; selection is queued
    // in store state and must not auto-dispatch a send on receipt.
    if (paths.length > 0) {
      currentView.value = 'Nearby';
    }
  },
  { deep: true },
);

watch(pendingPairingLink, (value) => {
  if (value) {
    // Pairing links can be handled in-place by Nearby / Trusted Devices, but
    // other views should fall back to Settings as the safe review surface.
    if (
      currentView.value !== 'Nearby' &&
      currentView.value !== 'TrustedDevices' &&
      currentView.value !== 'Settings'
    ) {
      currentView.value = 'Settings';
    }
  }
});

watch(pendingNavigationTarget, (target) => {
  if (!target) {
    return;
  }
  // Explicit shell navigation requests win over passive notices once emitted.
  currentView.value = target;
  clearPendingNavigationRequest();
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

const openNoticeTarget = (target: SystemNoticeTarget) => {
  currentView.value = target;
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
        :notice="systemError"
        @action="openNoticeTarget"
        @dismiss="dismissSystemError"
      />
      <div class="workspace-body">
        <NearbyView
          v-if="currentView === 'Nearby'"
          @open-settings="currentView = 'Settings'"
          @open-transfers="currentView = 'Transfers'"
        />
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
        <p class="text-muted">Verify the shared short code matches on both devices before trusting a new peer.</p>
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
  overflow: hidden;
}

.app-rail {
  width: 200px;
  flex-shrink: 0;
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

.rail-nav li {
  min-width: 0;
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
    flex-direction: column;
  }

  .app-rail {
    width: 100%;
    padding: 10px;
    gap: 10px;
  }

  .brand-title {
    font-size: 1.2rem;
  }

  .brand-block {
    margin-bottom: 0;
    padding-bottom: 0;
    border-bottom: 0;
  }

  .rail-nav {
    flex-direction: row;
    gap: 8px;
    overflow-x: auto;
    padding-bottom: 2px;
  }

  .rail-btn {
    white-space: nowrap;
  }
}

@media (max-width: 640px) {
  .app-shell {
    padding: 0;
    gap: 0;
  }

  .app-rail,
  .app-workspace {
    border-radius: 0;
    border-left: 0;
    border-right: 0;
  }

  .app-rail {
    border-top: 0;
    padding: 10px 12px 8px;
  }

  .app-workspace {
    border-bottom: 0;
  }

  .rail-nav {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    overflow-x: visible;
  }

  .rail-btn {
    text-align: center;
    padding: 9px 10px;
  }
}
</style>
