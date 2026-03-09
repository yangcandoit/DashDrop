<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{
  message: string;
}>();

const emit = defineEmits<{
  (e: 'dismiss'): void;
}>();

const parts = computed(() => {
  const [summaryRaw, nextRaw] = props.message.split(' Next: ');
  const summary = summaryRaw.trim();
  const next = nextRaw?.trim();
  const lower = summary.toLowerCase();

  let tone: 'error' | 'warning' | 'info' = 'info';
  let title = 'Notice';
  if (lower.includes('security warning') || lower.includes('fingerprint')) {
    tone = 'warning';
    title = 'Security Notice';
  } else if (lower.includes('failed') || lower.includes('error')) {
    tone = 'error';
    title = 'Transfer Notice';
  }

  return { summary, next, tone, title };
});
</script>

<template>
  <section class="notice-wrap">
    <article class="notice-card" :class="`tone-${parts.tone}`">
      <div class="notice-content">
        <p class="notice-title">{{ parts.title }}</p>
        <p class="notice-summary">{{ parts.summary }}</p>
        <p v-if="parts.next" class="notice-next">{{ parts.next }}</p>
      </div>
      <button class="notice-dismiss" type="button" @click="emit('dismiss')">Dismiss</button>
    </article>
  </section>
</template>

<style scoped>
.notice-wrap {
  padding: 14px 20px 0;
}

.notice-card {
  width: 100%;
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
  border-radius: 14px;
  border: 1px solid var(--border-subtle);
  background: rgba(255, 255, 255, 0.9);
  box-shadow: var(--shadow-card);
  padding: 12px 14px;
}

.notice-card.tone-error {
  border-color: rgba(157, 58, 51, 0.34);
  background: rgba(255, 248, 246, 0.95);
}

.notice-card.tone-warning {
  border-color: rgba(154, 93, 28, 0.34);
  background: rgba(255, 251, 242, 0.95);
}

.notice-title {
  margin: 0 0 4px;
  font-size: 0.72rem;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--text-subtle);
}

.notice-summary {
  margin: 0;
  font-size: 0.86rem;
  color: var(--text-secondary);
}

.notice-next {
  margin: 6px 0 0;
  font-size: 0.82rem;
  color: var(--text-muted);
}

.notice-dismiss {
  border: 1px solid var(--border-subtle);
  border-radius: 9px;
  background: #fffdf8;
  color: var(--text-secondary);
  padding: 6px 10px;
  font-size: 0.76rem;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  cursor: pointer;
}

.notice-dismiss:hover {
  border-color: var(--border-strong);
  color: var(--text-primary);
}

@media (max-width: 820px) {
  .notice-wrap {
    padding: 12px 12px 0;
  }

  .notice-card {
    flex-direction: column;
    gap: 10px;
  }
}
</style>
