<script lang="ts">
  let {
    checked,
    disabled = false,
    labelledby,
    onchange,
  }: { checked: boolean; disabled?: boolean; labelledby: string; onchange: (checked: boolean) => void } = $props()
</script>

<button
  class="switch"
  role="switch"
  aria-checked={checked}
  aria-labelledby={labelledby}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="knob" aria-hidden="true"></span>
</button>

<style>
  .switch {
    position: relative;
    flex: none;
    width: 40px;
    height: 20px;
    padding: 0;
    border: 1px solid var(--text-muted);
    border-radius: 10px;
    background: transparent;
    cursor: pointer;
    transition: background-color 120ms ease;
  }

  .knob {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--text-muted);
    transition: transform 120ms ease;
  }

  .switch[aria-checked='true'] {
    border-color: var(--accent);
    background: var(--accent);
  }

  .switch[aria-checked='true'] .knob {
    background: var(--accent-text);
    transform: translateX(20px);
  }

  .switch:disabled {
    opacity: 0.5;
    cursor: default;
  }

  @media (pointer: coarse) {
    .switch {
      /* Larger hit area without changing the drawn size. */
      outline-offset: 2px;
    }

    .switch::after {
      content: '';
      position: absolute;
      inset: -12px;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .switch,
    .knob {
      transition: none;
    }
  }
</style>
