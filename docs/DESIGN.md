# Conductor design rules

Conductor should feel like a well-made native utility: quiet, fast, obvious. Every screen is checked against this file before it ships.

## The idea

Your PCs are screens. Home is a wall of them, like monitors in a control room: each PC is its screen with a label underneath. The screen is the one bold thing on the page; everything around it stays quiet.

- **On, sharing its screen:** the latest picture of its main display.
- **On, not sharing:** a drawing of how its displays are arranged.
- **Asleep or off:** dark glass with a dim moon or power symbol, like a monitor's standby light.
- **Waking, restarting, asking before a power action:** written on the screen, the way a monitor shows its own messages.

The screen is dark in both themes, because it's glass.

## Principles

1. **The remote screen is the product.** In a session, nothing covers it unless the user asked for it.
2. **The device list is home.** No stats, charts, greetings or feature tours above it.
3. **Native text, our own labels.** Body text in the platform font (Segoe UI Variable on Windows, Roboto on Android, `system-ui` in browsers). PC names and page titles in Archivo, semi-condensed, like the label under a monitor. Follow the system light/dark setting. Platform-sized touch targets (48dp on Android).
4. **One accent color.** Teal, used for the primary action and focus, nothing else. Status colors (online, asleep, offline, error) are separate and never decorative.
5. **Progressive disclosure.** Defaults that work; advanced options one level down, never all at once.
6. **Every state is designed.** Connecting, reconnecting, waking, asleep, offline, relayed (slow path), access denied, update available. Not just the happy path.
7. **Plain words.** Name the machine and the action.

## Tokens

| Token | Light | Dark | Use |
|---|---|---|---|
| `--bg` | #eef1f0 | #111514 | Page. Neutrals lean slightly toward the accent. |
| `--surface` | #fbfcfc | #1a201f | Settings groups, menus, dialogs |
| `--text` / `--text-muted` | #141a19 / #56625f | #e5ebe9 / #97a3a0 | |
| `--accent` | #0a7a6c | #4cc2ae | Primary buttons, switches, focus |
| `--glass` | #131b1a | #050807 | A PC's screen |

Type sizes: 12 (notes), 14 (body), 18 (PC names), 20 (dialog titles), 28 (page titles). Two weights for body text (400, 600); titles 650.

## Surfaces

**Your PCs (home).** A grid of screens, about 300px wide and more as the window grows. Under each, the label: the name, a status dot with a line of status, and one action (Connect or Wake) as a plain button, so the screens stay the boldest thing on the page. Clicking the screen also connects. Power actions (sleep, restart, shut down, schedule, test waking) sit behind a small power button in the screen's top-right corner, which right-clicking the screen opens too. On a phone, or a window as narrow as one, each PC is a row: a small screen beside its label. An empty or failed list is one sentence and one button.

**Session view.** Video edge to edge. One small handle (draggable, hideable, recalled with a shortcut or a touch gesture) opens a compact panel: displays, keyboard, clipboard/files, sound, hide screen, quality, disconnect. Stats overlay is off by default. No toasts over the stream; transient status (reconnecting, relayed) appears as a small inline chip on the handle. On touch screens, where there's no hover to bring it back, the handle dims instead of hiding. The stream surface itself takes keyboard focus without a focus ring: it fills the window, every key goes to the remote PC, and a ring around the whole picture would cover its edges. The session view is always dark, whatever the system theme.

**Settings.** Windows 11 style: a panel per group, one setting per row with a line icon, the control on the right, and a one-line description only where the label is not self-explanatory. No nested cards.

**Dialogs.** Only for editing that needs room, like a PC's schedule. Questions before a power action are asked on that PC's screen, not in a dialog.

## AI-isms to avoid

### Visual
| Avoid | Instead |
|---|---|
| Indigo/violet/pink gradients on buttons or backgrounds | Solid fills in the one accent color |
| Gradient text | Solid color; hierarchy through size and weight |
| Inter/Geist everywhere at one weight | Platform font for text, Archivo for names and titles, a few sizes |
| Untinted zinc/slate grays, one radius everywhere, a border on every card | Slightly tinted neutrals; radius and borders scaled to element size |
| Glassmorphism, glows, orbs, neon edges, grid or stripe backgrounds | Plain solid surfaces |
| Border and soft shadow on the same element | Pick one. Shadows only on what floats over the page: menus and dialogs. In dark mode they also get a hairline edge, since a dark page swallows the shadow |
| Colored left stripe on cards | Only to mark a real status (error, offline) |
| Emoji or tinted icon tiles | One line-icon set, 16-20px, next to its label |
| Near-black page with a neon accent as the "modern" default | Follow the system theme; dark mode is a theme, not the brand |
| Pill badges for everything | Plain text or a small dot for status; badges only for counts |
| Shimmer skeletons for loads under ~300ms | Show nothing, then the content |

### Layout
| Avoid | Instead |
|---|---|
| Three identical icon/heading/text cards in a row | A list grouped by meaning. PCs are screens, not cards: no box around the screen and its label |
| Stat tiles at the top of the dashboard | The device list first; a number appears only if it changes a decision |
| Cards inside cards, the same spacing everywhere | Flat sections with dividers; more space between groups than within them |
| Eyebrow labels, "New" pills, 01/02/03 numbering | Remove unless it is a real sequence |
| A landing-page layout inside an app (centered max-width column, huge padding) | Use the window; dense, readable lists |
| Modals for everything, tabs with two items | Inline editing; a page when it needs space |
| Persistent header, sidebar or toolbar over the stream | The single auto-hiding handle |

### Copy
| Avoid | Instead |
|---|---|
| "Seamless", "effortless", "supercharge", "elevate", "unlock" | Say what happens: "Control MSI-1 from this phone" |
| Slogans and "not X, but Y" | Name the object and the action |
| Em dashes in most sentences | Periods and commas |
| "Oops! Something went wrong" | What failed and what to do: "Couldn't reach OFFICE-PC. It may be asleep. Wake it?" |
| Raw protocol errors ("ICE failed") | Human message first; technical detail behind "Details" |
| "Welcome back", "Get started by..." | One sentence, one button |
| Success toast on every action | Silence or an inline state change |
| "Are you sure?" with OK/Cancel | Named buttons: "Shut down" / "Cancel" |
| Title Case Everywhere, exclamation marks, "Let's" | Sentence case, calm tone |
| Tooltips that repeat the label | A tooltip only when it adds information (e.g. the shortcut) |

### Interaction
| Avoid | Instead |
|---|---|
| Hover zoom, bounce, wiggle, overshooting dialogs, confetti, count-up numbers | One short, purposeful transition at a time |
| Pulsing dots that never change | Animate only real activity (connecting, transferring) |
| Hover-only controls | Every hidden control has a tap, gesture or keyboard path |
| Spinners for instant actions, layout shift on load | Optimistic updates; reserve space for content |
| Disabled buttons with no reason | Say why, or hide it |
| Removed focus rings, unlabeled shortcuts | Visible focus; shortcuts shown in menus |

## References worth studying
- **Parsec**: one small draggable overlay button during a stream, hideable, recalled with a shortcut.
- **Moonlight / Steam Link**: stream overlays hidden behind a deliberate gesture; good touch precedent.
- **Windows 11 Settings**: native grouped-list settings pattern.
- **Broadcast multiviewers**: many screens on one wall, each labeled underneath.
- **Raycast, Linear**: restraint, one accent color, dense but calm lists.

## Review checklist (each milestone)
- [ ] Nothing on the stream that the user didn't open
- [ ] Every state above has a design and real copy
- [ ] Works by touch, mouse and keyboard
- [ ] Light and dark both checked; contrast passes WCAG AA
- [ ] No item from the AI-isms tables
- [ ] Reviewed twice: once by the main agent, once by an independent Haiku pass
