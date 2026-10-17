<script lang="ts">
  import Home from './views/Home.svelte'
  import Stream from './views/Stream.svelte'
  import type { HostInfo } from './lib/types'

  interface Target {
    url: string
    info: HostInfo
  }

  let target = $state<Target | null>(null)

  function open(t: Target) {
    target = t
    history.pushState({ stream: true }, '')
  }

  function close() {
    if (history.state?.stream) history.back()
    else target = null
  }

  $effect(() => {
    const onPop = () => (target = null)
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  })
</script>

{#if target}
  <Stream url={target.url} info={target.info} onclose={close} />
{:else}
  <Home onconnect={open} />
{/if}
