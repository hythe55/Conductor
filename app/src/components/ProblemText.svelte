<script lang="ts">
  import type { Problem } from '../lib/errors'

  let { problem }: { problem: Problem } = $props()
</script>

<p class="message">{problem.message}</p>
{#if problem.details}
  <details>
    <summary>Details</summary>
    <pre>{problem.details}</pre>
  </details>
{/if}

<style>
  .message {
    margin: 0;
  }

  details {
    font-size: var(--text-sm);
    color: var(--details-color, var(--text-muted));
  }

  summary {
    width: fit-content;
    padding: var(--space-1) 0;
    cursor: pointer;
  }

  pre {
    margin: var(--space-1) 0 0;
    max-width: 72ch;
    font: inherit;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    user-select: text;
  }

  @media (pointer: coarse) {
    summary {
      padding: var(--space-3) 0;
    }
  }
</style>
