<script setup lang="ts">
import { ref } from 'vue';
import NearbyView from './views/NearbyView.vue';
import SettingsView from './views/SettingsView.vue';

const currentView = ref('Nearby');
</script>

<template>
  <NearbyView v-if="currentView === 'Nearby'" @open-settings="currentView = 'Settings'" />
  <SettingsView v-if="currentView === 'Settings'" @back="currentView = 'Nearby'" />
</template>

<style>
/* Global resets over app wrapper if needed */
</style>