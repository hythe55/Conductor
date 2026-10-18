<script lang="ts">
  // One line-icon set for the whole app: 16px, 1.5px strokes in the text color.
  const paths = {
    back: 'M10 3.5 5.5 8l4.5 4.5',
    forward: 'M6 3.5 10.5 8 6 12.5',
    check: 'M3.5 8.5 6.5 11.5 12.5 4.5',
    more: 'M3.5 8h.01M8 8h.01M12.5 8h.01',
    settings: 'M2.5 5h6.5M12 5h1.5M2.5 11h1.5M7 11h6.5M10.5 3.5v3M5.5 9.5v3',
    sleep: 'M13 9.6A5.5 5.5 0 1 1 6.4 3a4.4 4.4 0 0 0 6.6 6.6Z',
    restart: 'M13 8a5 5 0 1 1-1.46-3.54M12 2v2.5H9.5',
    power: 'M8 2v5.5M11.5 4.2a5 5 0 1 1-7 0',
    clock: 'M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 1 0 0-11ZM8 5v3l2 1.5',
    wake: 'M8 5.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 1 0 0-5ZM8 1.5V3M8 13v1.5M1.5 8H3M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1',
    display: 'M2 3.5h12v8H2ZM5.5 14h5M8 11.5V14',
    eye: 'M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8ZM8 6a2 2 0 1 0 0 4 2 2 0 1 0 0-4Z',
    update: 'M8 2v8M4.5 6.5 8 10l3.5-3.5M2.5 13.5h11',
    info: 'M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 1 0 0-11ZM8 7.5V11M8 5.2v.1',
    file: 'M9 2H4v12h8V5ZM9 2v3h3',
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
