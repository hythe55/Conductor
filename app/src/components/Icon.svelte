<script lang="ts">
  // One line-icon set for the whole app: 16px, 1.5px strokes in the text color.
  const paths = {
    back: 'M10 3.5 5.5 8l4.5 4.5',
    forward: 'M6 3.5 10.5 8 6 12.5',
    check: 'M3.5 8.5 6.5 11.5 12.5 4.5',
    more: 'M3.5 8h.01M8 8h.01M12.5 8h.01',
  }
  // Dots need a heavier stroke to be seen.
  const weights: Partial<Record<keyof typeof paths, number>> = { more: 2.5 }

  let { name }: { name: keyof typeof paths } = $props()
</script>

<svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
  <path d={paths[name]} fill="none" stroke="currentColor" stroke-width={weights[name] ?? 1.5} stroke-linecap="round" stroke-linejoin="round" />
</svg>

<style>
  svg {
    flex: none;
    display: block;
  }
</style>
