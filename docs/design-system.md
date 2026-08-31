# Migo Design System

**One product, one identity, every screen size.**

This is the source of truth for Migo's visual language. The canonical machine-readable tokens
live in [`shared/design/tokens.json`](../shared/design/tokens.json) (v3); the web client reads them as
CSS custom properties in `clients/web/src/app/globals.css`, the Android client maps them into
`clients/android/app/src/main/kotlin/com/migo/app/ui/Theme.kt`, and the desktop client maps them
into `clients/desktop/src/theme.rs`. The `/design` route in the web client renders the whole
system — colours, type, spacing, icons, components, states — as living documentation.

The character: **compact, social, realtime, lightweight, modern, highly usable.** Dense rows,
small headers, short functional motion, one accent, one icon family. Not a SaaS dashboard, not
a card-based social clone. The v3 identity is the `docs/design/new-client-ui.tsx` reference's:
a teal-and-orange messenger with a top tab strip, an orange profile banner, closable chat tabs,
and a cream light surface.

## Colour

Two designed palettes — light and dark — switched by `<html data-theme>`. Dark is the home skin
and the default; light is a designed palette (the reference's cream), not an inversion.

| Role             | Light     | Dark      |
| ---------------- | --------- | --------- |
| Background       | `#fdfbf7` | `#0c1517` |
| Surface (panel)  | `#ffffff` | `#122023` |
| Surface (sunken) | `#f5f1e8` | `#1a2c30` |
| Border           | `#e8e2d4` | `#24393e` |
| Ink (primary)    | `#1e2b2e` | `#e9f4f5` |
| Ink (secondary)  | `#5c6a6d` | `#9db4b8` |
| Ink (tertiary)   | `#9aa5a7` | `#64808a` |
| Accent           | `#00838f` | `#00bcd4` |
| Accent (bright)  | `#00acc1` | `#26c6da` |
| Positive         | `#059669` | `#2fce7e` |
| Warning          | `#e67700` | `#f59f00` |
| Danger           | `#e03131` | `#ff5c7a` |
| Gold (badges)    | `#d97706` | `#fcc419` |

Two gradients sit above the themes and ignore them — the front door does not change with the
lights:

- **The banner gradient** (the profile banner): `#ea580c → #f97316 → #f59e0b`, carrying white
  ink. The active tab's underline is its middle stop.
- **The login gradient** (the sign-in screen): `#0093af → #00acc1 → #00838f`, carrying white
  ink. The submit button on it is the banner's orange.

Tints (hovers, soft fills, soft borders) are mixed from the accent at paint time
(`color-mix` on web) so they follow the accent across themes without a variable per tint.

## Typography

System fonts, no webfont. Base size 14px; the scale is small and dense on purpose.

| Step     | Size                         | Use                          |
| -------- | ---------------------------- | ---------------------------- |
| micro    | 11px, 600, uppercase, +0.4px | section headings, tab labels |
| meta     | 12px                         | timestamps, metadata         |
| body-sm  | 13px                         | secondary body, chips        |
| body     | 14px                         | messages, controls           |
| title-sm | 16px, 600                    | subtitles, banner name       |
| title    | 18px, 700                    | panel titles                 |
| display  | 22px, 700                    | the greeting                 |

## Spacing, radius, elevation

- Spacing: a 4px base — 4, 8, 12, 16, 20, 24, 32, 40, 48 (`--sp-1` … `--sp-12`).
- Radius: 4 (sm), 6 (md), 8 (lg), 12 (tab chip), 999 (pill). Message bubbles sit at **16px with
  a small corner tail** (top-left incoming, top-right outgoing); the composer's input is a
  **capsule**; the tab strip's chips are **12px rounded** with the active fill in accent-bright.
- Elevation: flat → raised (`0 2px 8px rgba(0,0,0,.08)`) → overlay; an accent glow for focus and
  the active state. Nothing floats without a reason.

## Icons

One family: 24×24 viewBox, 1.75 stroke, round caps and joins, `currentColor`. Sizes 16 / 20 / 24;
touch targets never smaller than 44×44 logical pixels. Web draws them as inline SVG
(`components/icons.tsx`); Android and desktop draw the same shapes to the same visual weight
with their own canvas (strokes, no icon dependency). Emoji are content (reactions), never chrome.

## Layout

One shell, one composition — the reference draws the same model at every size:

- The **tab strip** across the top: the system tabs **Friends, Chats, Rooms, Games, Feed**, then
  one closable chip per open conversation (and, on the desktop, per open panel). The active tab
  is the accent-bright fill with the orange underline; the strip is the `nav` token's surface.
- The **profile banner** under it: the orange gradient, the avatar (a white-ringed translucent
  disc), the name, the connection state, the $MIG balance — and the avatar's dropdown menu
  carrying **My Profile, My Credits & TopUp, Alerts, Search, Exit / Logout** (and Settings on
  the clients that have a settings panel).
- Content below, capped at a readable measure; a thread never hides the strip.

Breakpoints: 320 / 360 / 375 / 390 / 412 / 430 / 480 / 600 / 768 / 820 / 1024 / 1280 / 1440 / 1920+.

## Navigation

System tabs, in strip order: **Friends, Chats, Rooms, Games, Feed.** Dynamic tabs: **one per
open conversation**, closable at the chip. Panels — **Alerts, Search, Wallet, Profile,
Settings** — open from the banner's avatar menu (the desktop also gives them closable strip
chips). The same list on every platform, re-composed per size — never a different information
architecture for a different device.

## Components

The shared vocabulary: TabStrip, ProfileBanner, AvatarMenu, Avatar, PresenceIndicator, Badge,
Button, IconButton, Input, SearchInput, UserRow, ConversationRow, MessageBubble, MessageComposer,
RoomRow, RoomMessage, MemberList, ActivityRow, GamesPanel, TokenReference, ProfileHeader,
NotificationRow, SettingsRow, ContextMenu, BottomSheet, Dialog, Toast, Tooltip, Skeleton,
EmptyState, ErrorState.

Every component is responsive, themed by tokens, and carries its own ARIA semantics.

## Motion

120 / 180 / 240ms, functional and subtle (a sheet rises, a menu pops, a skeleton pulses).
`prefers-reduced-motion` collapses every animation to opacity-only or none.

## The coin

Migo's currency is **$MIG** — the one ticker this build recognises. In message text a
word-bounded `$MIG` renders as a chip that opens the Wallet; the banner's balance pill and the
coin badges carry the same mark.

## Rules

1. No random values: compose from the scales; a one-off number needs a reason in a comment.
2. One icon family, one spacing language, one interaction pattern per concept.
3. Compact beats airy: a row is 44px, the strip is 46px, the banner is 58px, a panel title is 18px.
4. Dark is designed, not inverted. The banner and login gradients are designed once, not twice.
5. Every screen works at 320px and at 1920px.
