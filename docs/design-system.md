# Migo Design System

**One product, one identity, every screen size.**

This is the source of truth for Migo's visual language. The canonical machine-readable tokens
live in [`shared/design/tokens.json`](../shared/design/tokens.json); the web client reads them as
CSS custom properties in `clients/web/src/app/globals.css`, the Android client maps them into
`clients/android/app/src/main/kotlin/com/migo/app/ui/Theme.kt`, and the desktop client maps them
into `clients/desktop/src/theme.rs`. The `/design` route in the web client renders the whole
system — colours, type, spacing, icons, components, states — as living documentation.

The character: **compact, social, realtime, lightweight, modern, highly usable.** Dense rows,
small headers, short functional motion, one accent, one icon family. Not a SaaS dashboard, not
a card-based social clone.

## Colour

Two designed palettes — light and dark — switched by `<html data-theme>`. Dark is the home skin
and the default; light is a designed palette, not an inversion.

| Role             | Light     | Dark      |
| ---------------- | --------- | --------- |
| Background       | `#f0f2f5` | `#0a0a12` |
| Surface (panel)  | `#ffffff` | `#111118` |
| Surface (sunken) | `#f5f6f8` | `#1a1a28` |
| Border           | `#e0e3e8` | `#1a1a2e` |
| Ink (primary)    | `#1a1d24` | `#e8e8f0` |
| Ink (secondary)  | `#5c6370` | `#8888a0` |
| Ink (tertiary)   | `#9aa1ad` | `#555570` |
| Accent           | `#0077e6` | `#00d4ff` |
| Positive         | `#00a85a` | `#00ff88` |
| Warning          | `#e6a100` | `#ffaa00` |
| Danger           | `#e04050` | `#ff4466` |
| Gold (badges)    | `#9a6700` | `#ffd166` |

Tints (hovers, soft fills, soft borders) are mixed from the accent at paint time
(`color-mix` on web) so they follow the accent across themes without a variable per tint.

## Typography

System fonts, no webfont. Base size 14px; the scale is small and dense on purpose.

| Step     | Size                         | Use                                 |
| -------- | ---------------------------- | ----------------------------------- |
| micro    | 11px, 600, uppercase, +0.4px | section headings, bottom-nav labels |
| meta     | 12px                         | timestamps, metadata                |
| body-sm  | 13px                         | secondary body, chips               |
| body     | 14px                         | messages, controls                  |
| title-sm | 16px, 600                    | subtitles, hero name                |
| title    | 18px, 700                    | panel titles                        |
| display  | 22px, 700                    | the greeting                        |

## Spacing, radius, elevation

- Spacing: a 4px base — 4, 8, 12, 16, 20, 24, 32, 40, 48 (`--sp-1` … `--sp-12`).
- Radius: 4 (sm), 6 (md), 8 (lg), 999 (pill).
- Elevation: flat → raised (`0 2px 8px rgba(0,0,0,.08)`) → overlay; an accent glow for focus and
  the active state. Nothing floats without a reason.

## Icons

One family: 24×24 viewBox, 1.75 stroke, round caps and joins, `currentColor`. Sizes 16 / 20 / 24;
touch targets never smaller than 44×44 logical pixels. Web draws them as inline SVG
(`components/icons.tsx`); Android and desktop use the closest system equivalents drawn to the
same visual weight. Emoji are content (reactions), never chrome.

## Layout

Three compositions of one shell:

- **Mobile** (<768px): 44px header, content, five-slot bottom bar (Home, Chats, Rooms, Space,
  More). More opens a bottom sheet carrying the remaining sections. With a chat thread open the
  global header folds away — the thread's own header is the only chrome that pane needs.
- **Tablet** (768–1023px): the rail collapses to icons.
- **Desktop** (≥1024px): full rail (icon + label) beside the content. Panels cap at 640–860px;
  content never stretches edge-to-edge on a monitor.

Breakpoints: 320 / 360 / 375 / 390 / 412 / 430 / 480 / 600 / 768 / 820 / 1024 / 1280 / 1440 / 1920+.

## Navigation

Primary sections, in order: **Home, Chats, Rooms, Space, Friends, Alerts, Search, Wallet.**
Secondary: **Profile, Settings.** The same list on every platform, re-composed per size — never
a different information architecture for a different device.

## Components

The shared vocabulary: AppShell, Avatar, PresenceIndicator, Badge, Button, IconButton, Input,
SearchInput, UserRow, ConversationRow, MessageBubble, MessageComposer, RoomRow, RoomMessage,
MemberList, SpacePost (activity row), TokenReference, ProfileHeader, NotificationRow,
SettingsRow, ContextMenu, BottomSheet, Dialog, Toast, Tooltip, Skeleton, EmptyState, ErrorState.

Every component is responsive, themed by tokens, and carries its own ARIA semantics.

## Motion

120 / 180 / 240ms, functional and subtle (a sheet rises, a menu pops, a skeleton pulses).
`prefers-reduced-motion` collapses every animation to opacity-only or none.

## The coin

Migo's currency is **$MIG** — the one ticker this build recognises. In message text a
word-bounded `$MIG` renders as a chip that opens the Wallet; the balance card and the coin
badges carry the same mark.

## Rules

1. No random values: compose from the scales; a one-off number needs a reason in a comment.
2. One icon family, one spacing language, one interaction pattern per concept.
3. Compact beats airy: a row is 44px, a header is 44px, a panel title is 18px.
4. Dark is designed, not inverted.
5. Every screen works at 320px and at 1920px.
