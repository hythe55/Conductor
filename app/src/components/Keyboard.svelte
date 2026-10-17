<script lang="ts">
  import type { ClientMsg } from '../lib/types'

  // The phone's own keyboard types into a hidden text box. Whatever changes in
  // the box is replayed on the remote PC: new text as text, removed text as
  // Backspace. That copes with autocorrect and word suggestions, which rewrite
  // text after it's typed. A row of keys the phone keyboard lacks sits above it.

  let { open = $bindable(false), send }: { open: boolean; send: (m: ClientMsg) => void } = $props()

  // Something to delete, so Backspace on an empty box still reaches the remote PC.
  const FILLER = '  '

  let box: HTMLTextAreaElement
  let last = FILLER
  let composing = false
  let held = $state<Set<string>>(new Set())

  /** Opens the phone keyboard. Call it from a tap, or the phone won't show it. */
  export function show() {
    reset()
    box.blur()
    box.focus()
    open = true
  }

  function reset() {
    box.value = FILLER
    last = FILLER
  }

  function tap(code: string) {
    const mods = [...held]
    for (const m of mods) send({ t: 'key', code: m, down: true })
    send({ t: 'key', code, down: true })
    send({ t: 'key', code, down: false })
    for (const m of mods.reverse()) send({ t: 'key', code: m, down: false })
    if (mods.length) held = new Set()
  }

  const PUNCTUATION: Record<string, string> = {
    ' ': 'Space',
    '-': 'Minus',
    '=': 'Equal',
    '[': 'BracketLeft',
    ']': 'BracketRight',
    '\\': 'Backslash',
    ';': 'Semicolon',
    "'": 'Quote',
    ',': 'Comma',
    '.': 'Period',
    '/': 'Slash',
    '`': 'Backquote',
  }

  function codeFor(ch: string): string | null {
    const c = ch.toLowerCase()
    if (c >= 'a' && c <= 'z') return `Key${c.toUpperCase()}`
    if (c >= '0' && c <= '9') return `Digit${c}`
    return PUNCTUATION[c] ?? null
  }

  function type(text: string) {
    let run = ''
    const flush = () => {
      if (run) send({ t: 'text', text: run })
      run = ''
    }
    for (const ch of text) {
      if (ch === '\n' || ch === '\t') {
        flush()
        tap(ch === '\n' ? 'Enter' : 'Tab')
      } else if (held.size) {
        // Ctrl+C and friends need the key itself, not the character.
        const code = codeFor(ch)
        flush()
        if (!code) {
          run = ch
          continue
        }
        if (ch !== ch.toLowerCase()) held = new Set([...held, 'ShiftLeft'])
        tap(code)
      } else {
        run += ch
      }
    }
    flush()
  }

  function onInput() {
    const now = box.value
    let same = 0
    while (same < last.length && same < now.length && last[same] === now[same]) same++
    for (let n = last.length - same; n > 0; n--) tap('Backspace')
    type(now.slice(same))
    last = now
    if (!composing && (now.length < FILLER.length || now.includes('\n') || now.length > 200)) reset()
  }

  function onKey(e: KeyboardEvent) {
    // Phone keyboards report most keys as "Unidentified" and type through onInput.
    // Keys from a real keyboard that don't type text are sent as they are.
    if (e.isComposing || e.key === 'Unidentified' || !e.code) return
    const typesText = e.key.length === 1 || e.key === 'Backspace' || e.key === 'Enter'
    if (typesText && !e.ctrlKey && !e.altKey && !e.metaKey) return
    e.preventDefault()
    send({ t: 'key', code: e.code, down: e.type === 'keydown' })
  }

  // The keyboard can be dismissed with the phone's own controls; notice when it goes.
  $effect(() => {
    const vv = window.visualViewport
    if (!vv || !open) return
    let seen = false
    const onResize = () => {
      const covered = window.innerHeight - vv.height > 100
      if (covered) seen = true
      else if (seen) box.blur()
    }
    vv.addEventListener('resize', onResize)
    return () => vv.removeEventListener('resize', onResize)
  })

  function toggle(code: string) {
    const next = new Set(held)
    if (next.has(code)) next.delete(code)
    else next.add(code)
    held = next
  }

  const keys: { label: string; name?: string; code: string; modifier?: boolean }[] = [
    { label: 'Esc', name: 'Escape', code: 'Escape' },
    { label: 'Tab', code: 'Tab' },
    { label: 'Ctrl', code: 'ControlLeft', modifier: true },
    { label: 'Alt', code: 'AltLeft', modifier: true },
    { label: 'Win', name: 'Windows key', code: 'MetaLeft', modifier: true },
    { label: '←', name: 'Left arrow', code: 'ArrowLeft' },
    { label: '↑', name: 'Up arrow', code: 'ArrowUp' },
    { label: '↓', name: 'Down arrow', code: 'ArrowDown' },
    { label: '→', name: 'Right arrow', code: 'ArrowRight' },
  ]
</script>

<textarea
  bind:this={box}
  class="box"
  tabindex="-1"
  aria-label="Type on the remote PC"
  autocomplete="off"
  autocapitalize="off"
  spellcheck="false"
  oninput={onInput}
  onkeydown={onKey}
  onkeyup={onKey}
  oncompositionstart={() => (composing = true)}
  oncompositionend={() => {
    composing = false
    onInput()
  }}
  onblur={() => {
    open = false
    held = new Set()
  }}
></textarea>

{#if open}
  <!-- Taps here must not take focus from the text box, or the phone keyboard closes. -->
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div class="keys" role="toolbar" aria-label="More keys" onpointerdown={(e) => e.preventDefault()}>
    {#each keys as k (k.code)}
      {#if k.modifier}
        <button
          class="key"
          tabindex="-1"
          aria-label={k.name}
          aria-pressed={held.has(k.code)}
          onmousedown={(e) => e.preventDefault()}
          onclick={() => toggle(k.code)}>{k.label}</button
        >
      {:else}
        <button
          class="key"
          tabindex="-1"
          aria-label={k.name}
          onmousedown={(e) => e.preventDefault()}
          onclick={() => tap(k.code)}>{k.label}</button
        >
      {/if}
    {/each}
  </div>
{/if}

<style>
  .box {
    position: absolute;
    top: 0;
    left: 0;
    width: 1px;
    height: 1px;
    padding: 0;
    border: 0;
    opacity: 0;
    resize: none;
    pointer-events: none;
  }

  .keys {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    gap: var(--space-1);
    padding: var(--space-1) max(var(--space-1), env(safe-area-inset-right)) var(--space-1)
      max(var(--space-1), env(safe-area-inset-left));
    overflow-x: auto;
    background: var(--overlay-solid);
    border-top: 1px solid var(--overlay-border);
  }

  .key {
    flex: 1 1 0;
    min-width: 0;
    height: 44px;
    padding: 0 var(--space-2);
    border: 0;
    border-radius: var(--radius-sm);
    background: var(--overlay-hover);
    color: var(--overlay-text);
    font: inherit;
    cursor: pointer;
  }

  .key[aria-pressed='true'] {
    background: var(--overlay-accent);
    color: var(--overlay-accent-text);
  }
</style>
