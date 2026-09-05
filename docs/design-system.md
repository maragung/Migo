# Migo Design System

**One product, one identity, every screen size.**

This is the source of truth for Migo's visual language. The canonical machine-readable tokens
live in [`shared/design/tokens.json`](../shared/design/tokens.json) (v4); the web client reads them as
CSS custom properties in `clients/web/src/app/globals.css`, the Android client maps them into
`clients/android/app/src/main/kotlin/com/migo/app/ui/Theme.kt`, and the desktop client maps them
into `clients/desktop/src/theme.rs`. The `/design` route in the web client renders the whole
system — colours, type, spacing, icons, components, states — as living documentation.

The character: **compact, social, realtime, lightweight, modern, highly usable.** Dense rows,
small headers, short functional motion, one accent, one icon family. Not a SaaS dashboard, not
a card-based social clone.

The v4 identity is a **flat modern restyle** of the mig33 lineage: solid colours only — no
gradients, no glossy highlights, no bevels or inset shadows, no text shadows. Separation comes
from 1px borders and a single soft elevation shadow, used only by things that genuinely float.
The composition is a **desktop-OS windowing metaphor**: a turquoise desk, a Contacts window,
one floating window per conversation, and a taskbar (a top tab strip on a phone). Lists are
flush hairline rows rather than cards; the transcript is a script, not a stack of bubbles;
each list closes with a pale status band. The base font size is **12px**.

## Colour

Two designed palettes — light and dark — switched by `<html data-theme>`. Light is the reference's
own pale teal; dark is a designed deep-teal skin, not an inversion.

| Role             | Light     | Dark      |
| ---------------- | --------- | --------- |
| Background       | `#eef7fa` | `#072a33` |
| Surface (panel)  | `#ffffff` | `#0c3a46` |
| Surface (sunken) | `#eef7fa` | `#114b5a` |
| Border           | `#cfe3ea` | `#1a5866` |
| Ink (primary)    | `#134e5e` | `#e6f4f8` |
| Ink (secondary)  | `#5f8a99` | `#a3c4cd` |
| Ink (tertiary)   | `#8fb0bb` | `#6f97a3` |
| Accent           | `#1287a0` | `#1fa5c0` |
| Accent (strong)  | `#0d6373` | `#157e94` |
| Nav strip        | `#0d4353` | `#06222a` |
| Positive         | `#3fce6b` | `#3fce6b` |
| Warning / away   | `#f5b83d` | `#f5b83d` |
| Danger / unread  | `#e5503c` | `#ff6a54` |
| Gold (credits)   | `#f0a912` | `#f7c13a` |
| Status band      | `#e5f4f7` | `#0a333e` |

Two surfaces sit above the themes and ignore them — the front door does not change with the
lights:

- **The me card** (the profile banner): flat `#f5820c`, white ink, its counter chips in `#d2690b`.
- **The auth screen**: a flat `#0f96ad` ground carrying a solid `#0b6f82` card, white ink, and the
  banner's orange on the submit button.

Both survive as three-stop "gradient" tokens whose stops are set to one colour each, so every
existing gradient call site paints a solid band without being rewritten.

Tints (hovers, soft fills, soft borders) are mixed from the accent at paint time
(`color-mix` on web) so they follow the accent across themes without a variable per tint.

### Nickname colours

The transcript names every line, and a name's colour is a hash of the name, so one person keeps
one colour down a busy room. The hash is a 31-multiplier polynomial —
`h = (h * 31 + name.charCodeAt(i)) >>> 0`, then `h % 8` — and it is **byte-identical on all three
clients**, so a person's colour identity survives moving between them. Only the eight-colour ramp
changes per theme (the light hues are chosen against white and half of them vanish on a dark
ground); each index keeps its hue, and only lightness moves. Your own lines take the fixed
`selfName` teal instead of joining the cycle.

## Typography

`'Segoe UI', Tahoma, Verdana, Geneva, sans-serif` — the reference's own stack, no webfont. Base
size 12px; the scale is small and dense on purpose.

| Step     | Size                           | Use                           |
| -------- | ------------------------------ | ----------------------------- |
| micro    | 10.5px, 700, uppercase, +0.4px | section headings, status band |
| body-sm  | 11px                           | secondary body, chips         |
| meta     | 11.5px                         | timestamps, metadata          |
| body     | 12px                           | messages, controls            |
| title-sm | 14px, 700                      | row names, banner name        |
| title    | 16px, 700                      | panel titles                  |
| display  | 20px, 700                      | the greeting                  |

## Spacing, radius, elevation

- Spacing: a 4px base — 4, 8, 12, 16, 20, 24, 32, 40, 48 (`--sp-1` … `--sp-12`).
- Radius: 4 (sm), 6 (md — inputs, the composer field, retro tabs), 9 (tab chips), 12 (window
  frames, lg), 16 (the auth card), 999 (pill). **List rows have no radius at all**: they are flush
  hairline rows, and a rounded row is a card.
- Elevation: exactly **two** shadows in the system — the frame
  (`0 12px 32px rgba(4,48,60,.16)`) and menus (`0 10px 24px rgba(4,48,60,.18)`). No glows: the
  `--glow*` tokens survive as `none` so their hundred call sites paint nothing. Focus is a **ring**
  — zero blur, zero offset, `0 0 0 3px var(--accent-glow)` — which is not an elevation, and is the
  one thing flatness is not allowed to cost keyboard users.

## Icons

One family: 24×24 viewBox, 1.75 stroke, round caps and joins, `currentColor`. Sizes 16 / 20 / 24;
touch targets never smaller than 44×44 logical pixels. Web draws them as inline SVG
(`components/icons.tsx`); Android and desktop draw the same shapes to the same visual weight
with their own canvas (strokes, no icon dependency). Emoji are content (reactions), never chrome.

## Layout

The reference is a **desktop-OS windowing metaphor**, and the clients now mean it literally.
A PC is a desk; a phone is the same desk seen through one slot at a time.

On a PC (≥768px): the turquoise desk (`#0f96ad`, deep-teal in dark) carries a faint brand
watermark top-left, and everything the user opens is a **window** — `.win-frame`: a 1px line
border, 12px radius, white body, the teal gloss title bar with min/max/close controls, and
cascade placement (~26×24px steps) for every window after the first. Windows drag by the
title bar, resize from the east/south/south-east handles, and stack last-click-wins.

- **Contacts is a window**, not a sidebar: teal nav pills (**Friends, Rooms, Feed**), the
  orange me bar (avatar in a presence-coloured ring with a white halo, blinking dot,
  click-to-edit status line, mail chip, away moon), then the flush hairline rows of the open
  list (58px; rooms 66px, carrying the occupancy bar) and the status band at its foot.
- **Every conversation, room and group chat is its own window** — one composer per window,
  nothing shared. Panels — **Alerts, Search, Wallet, Profile, Account, Settings, Admins,
  Store, Games** — are windows too (≈400×320; Store 430×386).
- The **taskbar** (34px, deep teal, bottom by default and dockable to top — the position
  persists) is the inventory: one button per window (green dot active, pale dot minimised),
  the real $MIG balance, the session timer, the clock, the logout.

On a phone (<768px) there is no taskbar and windows carry no chrome. A 46px scrollable strip
at the top holds the home tabs **Friends, Rooms, Feed** (only Feed closes; "+" reopens it),
a hairline divider, then one closable tab per window with an unread badge. The active window
or home view fills the slot below. Home opens with the me card, then the list; tapping a
friend or room raises a **bottom sheet** (18px top radius, drag handle, 54px action rows,
the primary action in orange) that carries the intent — send a message, join the room —
rather than navigating on the first touch.

Breakpoints: 320 / 360 / 375 / 390 / 412 / 430 / 480 / 600 / 768 / 820 / 1024 / 1280 / 1440 / 1920+.

## Navigation

The window list is the navigation. The Contacts window's tabs: **Friends, Rooms, Feed.**
Everything else is a window minted by an intent — a conversation opened, a panel chosen from
the me bar's menu — and closed by its own X or its taskbar/tab-strip control. The same
model on every platform, re-composed per size: a PC shows many windows at once, a phone
parks all but one. Escape closes the focused chat window; back on a phone closes the active
tab. The information architecture never changes between devices — only how many windows are
visible.

## The transcript

A script, not a stack of bubbles. Every line opens with the sender's nickname and a colon in that
sender's hashed colour, then the text, then a small trailing clock. What _is_ run-gated is the
avatar: a 24px gutter is reserved on every line and filled with a 22px disc only on the first of a
run, so the text column starts at one x whether or not a face is drawn. A day divider or an
interleaved system notice starts a new run.

Three things the reference does not draw are kept, because they carry meaning the layout cannot:
content-free **tombstones** for deleted messages, **read ticks** derived from `seq <= readUpTo`,
and **reply quotes** that render `[deleted]` when their target is gone.

## Components

The shared vocabulary: RetroWindow, ContactsWindow, Taskbar, MobileTabBar, MobileHome,
IntentSheet (UserIntent, RoomIntent, Me), ConfirmDialog, MigoBrand, Avatar, ListFooter,
PresenceIndicator, Badge, Button, IconButton, Input, SearchInput, UserRow, ConversationRow,
MessageLine, MessageComposer, RoomRow, MemberList, ActivityRow, GamesPanel, TokenReference,
ProfileHeader, NotificationRow, SettingsRow, ContextMenu, BottomSheet, Dialog, Toast, Tooltip,
Skeleton, EmptyState, ErrorState.

Every component is responsive, themed by tokens, and carries its own ARIA semantics.

## Motion

120 / 180 / 240ms, functional and subtle (a sheet rises, a menu pops, a skeleton pulses).
`prefers-reduced-motion` collapses every animation to opacity-only or none.

## The coin

Migo's currency is **$MIG** — the one ticker this build recognises. In message text a
word-bounded `$MIG` renders as a chip that opens the Wallet; the status band's balance and the
coin badges carry the same mark. A balance that failed to load says nothing rather than zero.

## Rules

1. No random values: compose from the scales; a one-off number needs a reason in a comment.
2. One icon family, one spacing language, one interaction pattern per concept.
3. Flat means flat: no gradient, no bevel, no glow, no text shadow. Two elevations, and a focus
   ring is a ring.
4. Nothing outside the token blocks names a colour. A raw hex in a component is a bug, and a
   theme that only works in light is the bug it causes.
5. Compact beats airy: a touch target is 44px, the strip is 46px, a list row is 58px, the me card
   is 71px, a panel title is 16px.
6. Dark is designed, not inverted. The me card and the auth screen are designed once, not twice.
7. Every screen works at 320px and at 1920px.
