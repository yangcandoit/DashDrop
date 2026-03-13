<script setup lang="ts">
import { computed } from 'vue';
import type { SystemNoticeState } from '../store';

const props = defineProps<{ notice: SystemNoticeState }>();
const emit = defineEmits<{
  (e: 'dismiss'): void;
  (e: 'action', target: NonNullable<SystemNoticeState['actionTarget']>): void;
}>();

const parts = computed(() => {
  const [summaryRaw, nextRaw] = props.notice.message.split(' Next: ');
  const summary = summaryRaw.trim();
  const next = nextRaw?.trim();
  const tone = props.notice.tone ?? 'info';

  return { summary, next, tone, actionLabel: props.notice.actionLabel, actionTarget: props.notice.actionTarget };
});
</script>

<template>
  <section class="notice-wrap">
    <article class="notice-card" :class="`tone-${parts.tone}`">
      <p class="notice-summary">
        {{ parts.summary }}
        <span v-if="parts.next" class="notice-next">{{ parts.next }}</span>
      </p>
      <div class="notice-actions">
        <button
          v-if="parts.actionLabel && parts.actionTarget"
          class="notice-action"
          type="button"
          @click="emit('action', parts.actionTarget)"
        >
          {{ parts.actionLabel }}
        </button>
        <button class="notice-dismiss" type="button" @click="emit('dismiss')">Dismiss</button>
      </div>
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

.notice-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.notice-action,
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

  .notice-actions {
    width: 100%;
    justify-content: flex-end;
  }
}
</style>
