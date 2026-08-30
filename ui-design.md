# MIGO — COMPLETE CROSS-PLATFORM UI/UX REDESIGN

## Unified Web + Desktop + Tablet + Android + iOS Experience

Redesign and implement the complete Migo frontend into a single, cohesive, production-quality cross-platform experience.

The goal is to create ONE Migo product experience that works consistently across:

- Mobile Web
- Android
- iOS
- Tablet
- Desktop Web
- Desktop applications if applicable

Do NOT create separate visual identities for each platform.

Use one unified Migo Design System, one component architecture, one information architecture, and one UX language.

The interface should intelligently adapt to screen size while maintaining the same identity and interaction principles.

---

# IMPORTANT — VISUAL REFERENCE MATERIAL

Before implementing the UI, inspect and study these visual/reference sources.

These are REFERENCE MATERIAL ONLY.

Do NOT copy branding, logos, copyrighted assets, names, proprietary artwork, or exact visual assets.

Use them to understand:

- Information density
- Compact mobile layouts
- Navigation hierarchy
- Contact-list design
- Chat-room organization
- Messaging UX
- Presence indicators
- Menus
- Tabs
- Profile presentation
- Social interaction patterns
- Small-screen information architecture
- Efficient use of limited screen space

## PRIMARY VISUAL REFERENCES

### Reference 1 — Historical mobile client / version archive

https://mymig33pc2.blogspot.com/p/mig33-official-versions-13.html

Study the sections describing the mobile and touch versions.

The archive specifically documents:

- Java MIDP client
- Version 4.6
- Touch Phones version
- Full mobile feature set
- Mobile-first usage

---

### Reference 2 — Mobile UI / feature documentation

https://mig33indonesian.blogspot.com/2013/06/mig33-original.html

Study this for the historical mobile interaction philosophy.

Pay particular attention to:

- Lightweight client
- Mobile chat
- Tabbed navigation
- Fast access to features
- Mobile gaming
- Low data usage
- Compact navigation

---

### Reference 3 — 240×320 Java mobile UI archive

https://mobile.phoneky.com/java-software/?q=Editor+Mod+MIG&v=3

Use this as a visual reference for the constraints of classic small-screen mobile applications.

Important:

The catalog contains entries such as:

- Mig33 v4.6
- Mig33v4.6
- Mig 33 v46
- Mig 33 v46 Touch

and identifies these applications around the 240×320 form factor.

Study the screenshots/previews available from these listings.

---

### Reference 4 — Historical UI feature documentation

https://mymig33pc2.blogspot.com/2012/01/feature-user-friendly-interface-color.html

Study the documented interaction model, especially:

- Contact list
- Presence sorting
- User status
- View profile
- Private chat
- Room chat
- Room tabs
- Settings tabs
- Popup notifications
- User activity
- Room interaction
- Compact menus
- Touch interactions

This source is especially useful for understanding the original information architecture.

---

### Reference 5 — Additional historical mobile screenshots

https://mig33-malinau.blogspot.com/2010/06/mig33-v430-officially-unreleased-mobile.html

Use the available screenshots as additional reference material for:

- Mobile navigation
- Chat interface
- Avatar placement
- Menus
- Status
- Theme
- Small-screen composition

Do NOT reproduce the original branding.

---

### Reference 6 — Additional room/chat screenshots

https://mig33-malinau.blogspot.com/2010/06/begini-begitu-kicking-flooding-gift-and-mix.html

Study the screenshots for:

- Room chat
- User lists
- Chat controls
- Emoticons
- Room tools
- Status
- Gift interactions
- Compact controls

Again, these are UX references only.

---

# REFERENCE ANALYSIS REQUIREMENT

Before writing new UI code:

1. Open the reference pages.
2. Inspect all available screenshots/images.
3. Identify recurring UI patterns.
4. Identify information hierarchy.
5. Identify compact-layout techniques.
6. Identify navigation patterns.
7. Identify chat-room patterns.
8. Identify contact-list patterns.
9. Identify profile patterns.
10. Identify menu patterns.
11. Identify status/presence patterns.
12. Identify how limited screen space was utilized.

Then translate those principles into an ORIGINAL Migo Design System.

Do not blindly copy any single screenshot.

Synthesize the useful interaction principles into a modern design.

---

# DESIGN TARGET

The final result should feel like:

**A modern, realtime, lightweight social communication platform with a highly efficient classic mobile information architecture.**

It should NOT feel like:

- Generic SaaS dashboard
- Generic modern social media
- Generic WhatsApp clone
- Generic Discord clone
- Generic Telegram clone
- Oversized card-based application
- Excessive glassmorphism
- AI-generated template UI

The interface must have its own Migo identity.

---

# CORE DESIGN CHARACTERISTICS

Prioritize:

- Compact
- Fast
- Dense
- Clear
- Social
- Realtime
- Lightweight
- Responsive
- Easy to navigate
- Easy to scan
- Low bandwidth
- One-handed mobile interaction

Avoid:

- Huge headers
- Huge cards
- Excessive whitespace
- Excessive rounded containers
- Excessive gradients
- Excessive shadows
- Excessive animations
- Decorative UI without function

---

# ONE UNIFIED DESIGN SYSTEM

Create ONE Migo design system.

The same design tokens must drive:

- Mobile
- Tablet
- Desktop
- Android
- iOS
- Web

Responsive layouts may change structure, but the design language must remain recognizable.

Example:

Mobile:

┌────────────────────────────┐
│ ← Chat ⋮ │
├────────────────────────────┤
│ messages │
│ │
│ │
├────────────────────────────┤
│ + Message... ➤ │
└────────────────────────────┘

Tablet:

┌──────────┬─────────────────────────┐
│ Nav │ Chat │
│ │ │
│ Home │ messages │
│ Friends │ │
│ Rooms │ │
│ Space │ │
└──────────┴─────────────────────────┘

Desktop:

┌──────────┬──────────────────────┬───────────────┐
│ Nav │ Main │ Context │
│ │ │ │
│ Home │ Conversation │ Members │
│ Friends │ Room │ Profile │
│ Messages │ Space │ Details │
│ Rooms │ │ Media │
│ Space │ │ │
└──────────┴──────────────────────┴───────────────┘

These are different compositions of the SAME product.

---

# RESPONSIVE BREAKPOINTS

Support at minimum:

320px
360px
375px
390px
412px
430px
480px
600px
768px
820px
1024px
1280px
1440px
1920px+

Do not assume only one mobile size.

---

# MOBILE

Mobile must be designed as a first-class experience.

Use:

- Bottom navigation
- Compact header
- Full-screen conversations
- Bottom sheets
- Contextual menus
- Swipe navigation
- Touch-friendly controls
- Safe-area handling

Do NOT simply shrink desktop.

---

# TABLET

Tablet should transition naturally between mobile and desktop.

Use:

Navigation + Content

or:

Navigation + Content + Context

depending on available width.

---

# DESKTOP

Desktop should use available space intelligently.

Preferred structure:

Navigation | Main | Context

The main area must remain visually focused.

Do not stretch content across the entire monitor.

Use reasonable maximum widths.

---

# PRIMARY NAVIGATION

Primary destinations:

- Home
- Explore
- Search
- Friends
- Messages
- Rooms
- Space
- Notifications
- Profile

Secondary:

- Wallet
- Settings
- Help
- About

Do not overcrowd mobile navigation.

Adapt navigation according to screen size while preserving the same information architecture.

---

# HOME

Home should be a compact realtime dashboard.

Include:

- Current profile
- Presence
- Recent messages
- Online friends
- Recent rooms
- Notifications
- Community activity
- Space activity
- Recommended users
- Recommended rooms
- Relevant token activity

Use compact sections.

---

# FRIENDS

Friend list should be extremely efficient.

Each row:

Avatar
Presence
Username
Status
Last activity
Unread indicator

Support:

- Search
- Online
- Offline
- Favorites
- Recent
- Requests

Interactions:

Tap → Profile

Long press → Context menu

Right click → Context menu on desktop

---

# PRIVATE MESSAGES

Conversation list:

Avatar
Username
Last message
Timestamp
Unread count
Presence
Pinned
Muted

Chat:

Header
Messages
Composer

Messages support:

- Text
- Emoji
- Images
- Video
- Audio
- Files
- Stickers
- Replies
- Reactions
- Mentions
- Editing
- Deletion
- Forwarding
- Pinning
- Search

---

# CHAT COMPOSER

Keep it extremely compact.

Mobile:

┌────────────────────────────┐
│ + Message... 😊 ➤ │
└────────────────────────────┘

Support:

- Attachments
- Emoji
- Voice
- Reply
- Drafts
- Multiline
- Mentions

Keyboard must never cover the composer.

---

# PUBLIC ROOMS

Rooms are a major Migo feature.

Room header:

- Back
- Room name
- Member count
- Online count
- Search
- Menu

Room messages:

Username
Timestamp
Message
Reactions
Mentions
System messages

Support large rooms with virtualized rendering.

---

# ROOM DIRECTORY

Categories:

- Popular
- Trending
- New
- Recommended
- Local
- Gaming
- Technology
- Crypto
- Music
- Entertainment
- Communities

Each row:

Icon
Room name
Description
Members
Online count
Category

Keep it compact.

---

# SPACE

Space is Migo's social feed.

Support:

- Posts
- Replies
- Likes
- Reposts
- Quotes
- Shares
- Mentions
- Hashtags
- Media

Keep posts information-dense.

Avoid giant social-media cards.

---

# TOKEN REFERENCES

Support:

$TICKER

and token contract addresses.

When a valid token is detected, show:

Token name
Ticker
Price
24h change
Mini chart

Click → Token page.

Use autocomplete when appropriate.

---

# PROFILE

Profile:

Avatar
Username
Presence
Bio
Friends
Followers
Following
Posts
Rooms
Media
Tokens

Actions:

Message
Add Friend
Follow
Mute
Block
Report

Keep profile information compact.

---

# SEARCH

Unified search:

Users
Friends
Messages
Rooms
Space
Tokens
Contracts

Support:

- Instant search
- Suggestions
- Filters
- Recent searches
- Debouncing

---

# NOTIFICATIONS

Support:

- Messages
- Friend requests
- Mentions
- Replies
- Reactions
- Room activity
- Community activity
- System events

Use compact notification rows.

---

# WALLET

Wallet must use the SAME Migo design language.

Support:

- Balance
- Assets
- Tokens
- Transactions
- Send
- Receive
- Address
- QR
- Network
- Token detail

Do not make wallet look like an unrelated application.

---

# SETTINGS

Categories:

Account
Privacy
Security
Notifications
Appearance
Messages
Rooms
Space
Wallet
Network
Storage
Accessibility
About

Use compact list navigation.

---

# CONTEXT MENUS

Mobile:

- Long press
- 3-dot
- Bottom sheet

Desktop:

- 3-dot
- Right click
- Context menu

Menus should be short and contextual.

---

# DESIGN TOKENS

Create centralized tokens for:

Colors
Typography
Spacing
Radius
Borders
Shadows
Elevation
Icon sizes
Touch targets
Animation
Z-index

No random hardcoded values.

---

# TYPOGRAPHY

Use readable system fonts.

Prioritize:

- Username
- Message
- Timestamp
- Metadata

Avoid oversized typography.

---

# ICONOGRAPHY

Use one consistent icon family.

Do not mix icon styles.

Primary icons:

20–24px

Touch target:

minimum 44×44 logical pixels.

---

# THEMES

Support:

Light
Dark
System

Dark mode must be intentionally designed.

Do not simply invert colors.

---

# ANIMATION

Animations must be:

- Short
- Functional
- Subtle

Support reduced motion.

Avoid decorative animations.

---

# ACCESSIBILITY

Implement:

- Semantic HTML
- ARIA
- Keyboard navigation
- Screen readers
- Focus states
- High contrast
- Reduced motion
- Accessible dialogs
- Accessible menus
- Accessible forms

---

# REALTIME

Realtime UI should support:

- Presence
- Messages
- Typing
- Read state
- Reactions
- Room activity
- Notifications

Optimize rendering so realtime events do not cause unnecessary full-page rerenders.

---

# OFFLINE / RECONNECT

Display subtle connection states:

Connected
Connecting
Reconnecting
Offline

Do not interrupt users with unnecessary dialogs.

---

# PERFORMANCE

Prioritize:

- Fast startup
- Small bundle
- Lazy loading
- Virtualized lists
- Image optimization
- Efficient WebSocket usage
- Pagination
- Caching
- Minimal rerenders
- Low memory usage

The application must work well on low-end mobile hardware.

---

# LOW BANDWIDTH

Prioritize text and essential UI.

Load media progressively.

Use:

- Thumbnail-first loading
- Lazy loading
- Compression
- Caching

Do not automatically download large media.

---

# PWA / MOBILE WEB

Mobile Web must behave like an application.

Support:

- PWA
- Manifest
- Service worker
- Offline shell
- Installability
- Push notifications where supported
- Safe areas
- Keyboard handling

---

# COMPONENT ARCHITECTURE

Create reusable components:

AppShell
Header
Sidebar
BottomNavigation
Avatar
PresenceIndicator
Badge
Button
IconButton
Input
SearchInput
UserRow
FriendRow
ConversationRow
MessageBubble
MessageComposer
RoomRow
RoomMessage
RoomHeader
MemberList
SpacePost
TokenReference
ProfileHeader
NotificationRow
SettingsRow
ContextMenu
BottomSheet
Dialog
Toast
Tooltip
Skeleton
EmptyState
ErrorState

Every component must be responsive.

---

# DESIGN SYSTEM DOCUMENTATION

Create a Design System page containing:

- Color tokens
- Typography
- Spacing
- Buttons
- Inputs
- Lists
- Headers
- Navigation
- Messages
- Rooms
- Profiles
- Notifications
- Dialogs
- Bottom sheets
- Loading states
- Empty states
- Error states

This becomes the source of truth for future Migo development.

---

# IMPLEMENTATION PROCESS

Before changing the code:

1. Inspect existing frontend.
2. Inspect routing.
3. Inspect state management.
4. Inspect API.
5. Inspect WebSocket/realtime system.
6. Inspect authentication.
7. Inspect current components.
8. Inspect existing responsive behavior.
9. Inspect chat.
10. Inspect rooms.
11. Inspect Space.
12. Inspect wallet.
13. Identify reusable code.
14. Identify UI that should be refactored.
15. Preserve existing working backend functionality.

DO NOT rebuild the backend just for the redesign.

DO NOT unnecessarily replace working infrastructure.

---

# IMPLEMENTATION ORDER

Phase 1:

Design tokens
↓
Typography
↓
Icons
↓
Base components
↓
Layout system

Phase 2:

AppShell
↓
Navigation
↓
Responsive layout

Phase 3:

Home
Friends
Messages
Chat
Rooms
Space
Profile
Notifications

Phase 4:

Search
Wallet
Settings
Secondary features

Phase 5:

Dark mode
Accessibility
PWA
Performance
Offline/reconnect

Phase 6:

Full responsive QA.

---

# VISUAL QA

For every major screen test:

320×568
360×640
375×667
390×844
412×915
430×932
768×1024
1024×768
1280×800
1440×900
1920×1080

Verify:

- No horizontal overflow
- No clipped text
- No overlapping controls
- No keyboard overlap
- Correct safe areas
- Correct scrolling
- Correct focus
- Correct touch targets
- Correct desktop behavior
- Correct tablet behavior
- Correct mobile behavior
- Correct dark mode

---

# CRITICAL RULE

Do not make the interface look like a collection of modern UI templates.

The entire application must feel intentionally designed as ONE product.

Every screen must share:

- Same spacing language
- Same typography
- Same iconography
- Same interaction patterns
- Same navigation logic
- Same component behavior
- Same visual hierarchy

The result should feel:

**compact + social + realtime + lightweight + modern + highly usable.**

---

# FINAL OBJECTIVE

Build Migo as a unified cross-platform communication and social platform.

It should feel like:

**one application that intelligently adapts itself to every screen size.**

Not:

"mobile version + desktop version + tablet version."

Instead:

**Migo everywhere, with one consistent experience.**

Use the historical references above only as UX inspiration for compact information architecture and efficient interaction design.

Create a completely original Migo visual identity.
