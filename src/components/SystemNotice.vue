<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{ message: string }>();
const emit = defineEmits<{ (e: 'dismiss'): void }>();

const parts = computed(() => {
  const [summaryRaw, nextRaw] = props.message.split(' Next: ');
  const summary = summaryRaw.trim();
  const next = nextRaw?.trim();
  const lower = summary.toLowerCase();

  let tone: 'error' | 'warning' | 'info' = 'info';
  if (lower.includes('security warning') || lower.includes('fingerprint')) {
    tone = 'warning';
  } else if (lower.includes('failed') || lower.includes('error')) {
    tone = 'error';
  }

  return { summary, next, tone };
});
</script>

<template>
  <section class="notice-wrap">
    <article class="notice-card" :class="`tone-${parts.tone}`">
      <p class="notice-summary">
        {{ parts.summary }}
        <span v-if="parts.next" class="notice-next">{{ parts.next }}</span>
      </p>
      <button class="notice-dismiss" type="button" @click="emit('dismiss')">Dismiss</button>
    </article>
  </section>
</template>

<style scoped>
.notice-wrap {
  padding: 10px 12px 0;
}

.notice-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 8px 10px;
  border-radius: 10px;
  border: 1px solid var(--border-subtle);
  background: #f7f7f8;
}

.notice-card.tone-error {
  background: #fff5f5;
  border-color: rgba(198, 40, 40, 0.24);
}

.notice-card.tone-warning {
  background: #fff8ef;
  border-color: rgba(178, 106, 0, 0.24);
}

.notice-summary {
  margin: 0;
  font-size: 0.85rem;
  color: var(--text-secondary);
}

.notice-next {
  color: var(--text-muted);
}

.notice-dismiss {
  border: 1px solid var(--border-subtle);
  border-radius: 8px;
  background: #fff;
  color: var(--text-secondary);
  padding: 4px 8px;
  font-size: 0.76rem;
  font-weight: 600;
  cursor: pointer;
}

@media (max-width: 820px) {
  .notice-card {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
