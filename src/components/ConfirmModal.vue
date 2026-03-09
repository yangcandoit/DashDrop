<script setup lang="ts">
const props = withDefaults(
  defineProps<{
    open: boolean;
    title: string;
    message: string;
    confirmText?: string;
    cancelText?: string;
    tone?: "default" | "danger";
  }>(),
  {
    confirmText: "Confirm",
    cancelText: "Cancel",
    tone: "default",
  },
);

const emit = defineEmits<{
  (e: "confirm"): void;
  (e: "cancel"): void;
}>();
</script>

<template>
  <div v-if="props.open" class="dialog-backdrop" @click.self="emit('cancel')">
    <section class="dialog-card">
      <h3>{{ props.title }}</h3>
      <p class="text-muted dialog-copy">{{ props.message }}</p>
      <div class="dialog-actions">
        <button class="btn btn-secondary" @click="emit('cancel')">{{ props.cancelText }}</button>
        <button
          class="btn"
          :class="props.tone === 'danger' ? 'btn-danger' : 'btn-primary'"
          @click="emit('confirm')"
        >
          {{ props.confirmText }}
        </button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.dialog-backdrop {
  position: absolute;
  inset: 0;
  background: rgba(33, 30, 24, 0.38);
  backdrop-filter: blur(6px);
  display: flex;
  justify-content: center;
  align-items: center;
  padding: 18px;
  z-index: 50;
}

.dialog-card {
  width: min(460px, 100%);
  border-radius: 16px;
  border: 1px solid var(--border-subtle);
  background: #fff;
  box-shadow: var(--shadow-soft);
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.dialog-copy {
  font-size: 0.9rem;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

@media (max-width: 860px) {
  .dialog-actions {
    width: 100%;
  }

  .dialog-actions .btn {
    flex: 1;
  }
}
</style>
