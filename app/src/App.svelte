<script lang="ts">
  import Home from './views/Home.svelte'
  import Settings from './views/Settings.svelte'
  import Stream from './views/Stream.svelte'
  import type { HostInfo } from './lib/types'

  interface Target {
    url: string
    info: HostInfo
  }

  type View = { name: 'home' } | { name: 'settings' } | { name: 'stream'; target: Target }

  let view = $state<View>({ name: 'home' })

  // Each screen gets a history entry so the back button and gesture work.
  function go(next: View) {
    view = next
    history.pushState({ view: next.name }, '')
  }

  function back() {
    if (history.state?.view) history.back()
    else view = { name: 'home' }
  }

  $effect(() => {
    const onPop = () => (view = { name: 'home' })
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  })
</script>

{#if view.name === 'stream'}
  <Stream url={view.target.url} info={view.target.info} onclose={back} />
{:else if view.name === 'settings'}
  <Settings onback={back} />
{:else}
  <Home onconnect={(target) => go({ name: 'stream', target })} onsettings={() => go({ name: 'settings' })} />
{/if}
