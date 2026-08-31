# MIGO — COMPLETE CROSS-PLATFORM MESSENGER UI/UX REDESIGN

Redesign the entire Migo client UI/UX.

IMPORTANT:
The current implementation looks like a generic modern SaaS dashboard with a large permanent sidebar and excessive empty space.

THIS IS WRONG.

The target experience must be a:

- Messenger
- Contact/social client
- Private chat application
- Public chat-room application
- Realtime community application

It must NOT look like:

- SaaS dashboard
- Admin panel
- Productivity application
- Generic AI-generated dashboard
- Generic Discord clone
- Generic Telegram clone
- Generic WhatsApp clone

The primary visual and UX identity must be based on a compact, information-dense, classic mobile-messenger interaction model, but redesigned as an original modern Migo product.

Do not mention or copy any existing product branding, logo, artwork, proprietary assets, or exact copyrighted UI.

Use the following historical screenshots/references ONLY to understand:

- information density
- contact-list architecture
- presence indicators
- chat interaction
- room interaction
- tabs
- compact menus
- profile presentation
- mobile navigation
- efficient use of screen space

REFERENCE MATERIAL:

https://mymig33pc2.blogspot.com/p/mig33-official-versions-13.html

https://mig33indonesian.blogspot.com/2013/06/mig33-original.html

https://mobile.phoneky.com/java-software/?q=Editor+Mod+MIG&v=3

https://mymig33pc2.blogspot.com/2012/01/feature-user-friendly-interface-color.html

https://mig33-malinau.blogspot.com/2010/06/mig33-v430-officially-unreleased-mobile.html

https://mig33-malinau.blogspot.com/2010/06/begini-begitu-kicking-flooding-gift-and-mix.html

OPEN AND STUDY THESE REFERENCES BEFORE IMPLEMENTING THE DESIGN.

Do not blindly copy screenshots.

Extract the underlying UX principles and create an ORIGINAL Migo interface.

---

# 1. CRITICAL DESIGN DIRECTION

The application must immediately feel like a communication client.

When the user opens Migo, the first impression should be:

PEOPLE
↓
PRESENCE
↓
CONVERSATIONS
↓
ROOMS
↓
SOCIAL ACTIVITY

NOT:

SIDEBAR
↓
EMPTY DASHBOARD
↓
LARGE CARDS

The current design with:

"Home
Chats
Rooms
Space
Friends
Alerts
Search
Wallet
Profile
Settings"

inside a large permanent sidebar with a mostly empty content area is NOT acceptable.

Remove this SaaS-dashboard visual approach.

---

# 2. MESSENGER-FIRST INFORMATION ARCHITECTURE

Migo's primary experience should revolve around:

1. Friends / Contacts
2. Conversations
3. Rooms
4. Space / Social
5. Notifications
6. Search
7. Profile
8. Wallet
9. Settings

Friends, Chats, and Rooms must be visually prominent.

They should NOT feel like secondary navigation items hidden behind a dashboard.

---

# 3. ONE UNIFIED EXPERIENCE

Migo must work across:

- Android
- iOS
- Mobile Web
- Tablet
- Desktop Web

Use ONE design system.

Do not create completely different interfaces for each platform.

The composition may adapt.

The UX principles must remain identical.

Example:

MOBILE:
full-screen contact/chat/room

TABLET:
navigation + main content

DESKTOP:
navigation/context + conversations + details

All must still clearly feel like the same Migo application.

---

# 4. DO NOT USE A LARGE PERMANENT SAAS SIDEBAR

A large 240–300px sidebar must NOT dominate the application.

Do not create:

┌───────────────┬─────────────────────────────┐
│ Home │ │
│ Chats │ │
│ Rooms │ │
│ Space │ HUGE EMPTY AREA │
│ Friends │ │
│ Alerts │ │
│ Search │ │
│ Wallet │ │
│ │ │
│ Profile │ │
│ Settings │ │
└───────────────┴─────────────────────────────┘

This pattern is specifically prohibited.

Instead, use an adaptive messenger-oriented layout.

---

# 5. MOBILE PRIMARY EXPERIENCE

Mobile should be the strongest representation of Migo.

Example:

┌──────────────────────────────────┐
│ Migo ⋮ │
├──────────────────────────────────┤
│ FRIENDS CHATS ROOMS │
├──────────────────────────────────┤
│ │
│ 🟢 Alex │
│ Hey, are you online? │
│ │
│ 🟢 Sarah │
│ Let's join the room │
│ │
│ 🟡 Mike │
│ Away │
│ │
│ ⚪ John │
│ Offline │
│ │
├──────────────────────────────────┤
│ Home Friends Chat Rooms More │
└──────────────────────────────────┘

The actual implementation should be visually polished and modern, but preserve this information hierarchy.

The user should see people and activity immediately.

---

# 6. CONTACT / FRIENDS EXPERIENCE

Friends are a core part of Migo.

Create a compact contact list.

Every user row can contain:

- Avatar
- Online/offline indicator
- Username
- Status message
- Last activity
- Unread indicator where relevant
- Optional favorite indicator

Example:

┌──────────────────────────────────┐
│ 🟢 Alex │
│ Available │
├──────────────────────────────────┤
│ 🟢 Sarah │
│ Hey everyone 👋 │
├──────────────────────────────────┤
│ 🟡 Mike │
│ Busy │
└──────────────────────────────────┘

Do NOT turn every contact into a large card.

Use compact rows.

The contact list should be fast to scan.

---

# 7. PRESENCE SYSTEM

Presence must be highly visible.

Support:

- Online
- Away
- Busy
- Invisible
- Offline
- Do Not Disturb
- Custom status

Use small visual indicators.

Presence should appear consistently:

- Friends
- Chat list
- Chat header
- Profile
- Room members
- Search results

---

# 8. CHAT LIST

Chats should look like a real messenger conversation list.

Example:

┌──────────────────────────────────┐
│ Chats 🔍 │
├──────────────────────────────────┤
│ 🟢 Alex 2m │
│ See you later! 2 │
├──────────────────────────────────┤
│ 🟢 Sarah 10m │
│ Hello 👋 │
├──────────────────────────────────┤
│ 🟡 Mike 1h │
│ Are you coming? │
└──────────────────────────────────┘

Each row:

- Avatar
- Presence
- Username
- Last message
- Timestamp
- Unread count
- Muted/pinned state

Keep it compact.

---

# 9. PRIVATE CHAT

Private chat must be one of the most polished parts of the application.

Header:

┌──────────────────────────────────┐
│ ← 🟢 Alex ⋮ │
│ Online │
├──────────────────────────────────┤

Messages:

│ Alex  
│ Hello!  
│  
│ Hi Alex!  
│  
│ How are you?  
│  
├──────────────────────────────────┤
│ + Message... 😊 ➤ │
└──────────────────────────────────┘

Support:

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
- Copy
- Message search

The message area must use available screen space efficiently.

Avoid excessive whitespace.

---

# 10. MESSAGE COMPOSER

The composer must feel like a real messenger input.

Mobile:

- | Message... | Emoji | Send

Desktop:

Attachment | Message... | Emoji | Voice | Send

Requirements:

- Multiline
- Auto-growing
- Draft persistence
- Reply mode
- Attachment menu
- Emoji picker
- Voice recording if supported
- Mention autocomplete
- Keyboard-safe
- Safe-area aware

The composer must never be hidden behind the mobile keyboard.

---

# 11. PUBLIC CHAT ROOMS

Rooms are a primary Migo feature.

Room experience should be optimized for fast realtime conversation.

Example:

┌──────────────────────────────────┐
│ ← # Indonesia ⋮ │
│ 1,284 members │
├──────────────────────────────────┤
│ 🟢 alex: hello everyone │
│ 🟢 sarah: hi 👋 │
│ 🟡 mike: what's happening? │
│ 🟢 john: welcome! │
│ │
│ [system] user joined the room │
│ │
├──────────────────────────────────┤
│ + Message... ➤ │
└──────────────────────────────────┘

Room header should contain:

- Room name
- Room icon
- Member count
- Online count
- Search
- Room actions
- More menu

---

# 12. ROOM DIRECTORY

Create a compact room browser.

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

Example:

# Popular Rooms

🌐 Indonesia 12.4K
🎮 Gaming 8.2K
₿ Crypto 3.4K
🎵 Music 2.1K

Use compact list rows.

Do not create huge cards.

---

# 13. ROOM MEMBERS

Room member list should support:

- Avatar
- Presence
- Username
- Role
- Moderator indicator
- Status

Support:

- Search members
- Online members
- Moderators
- Context menu

Long press on mobile.

Right click on desktop.

---

# 14. TABS

Use tabs where they improve information density.

Examples:

Friends | Chats | Rooms

or:

Messages | Media | Files

or:

Posts | Replies | Media

Tabs should be compact and easy to switch.

Do not overuse tabs.

---

# 15. HOME

Home should NOT be a giant dashboard.

Home should act as a compact activity hub.

Possible sections:

Recent chats
Online friends
Joined rooms
Recent activity
Notifications
Space activity

The user should immediately see active people and conversations.

Avoid:

Large hero sections
Large empty areas
Huge analytics cards
Dashboard widgets

---

# 16. SPACE

Space is Migo's social layer.

Space should still feel like part of the messenger ecosystem.

Users can:

- Post
- Reply
- Like
- Repost
- Quote
- Share
- Mention
- Follow
- Hashtag
- Attach media

Posts should be compact.

Example:

🟢 Alex · 2m

Had a great conversation today.

$MIGO $BTC

♡ 24 💬 8 ↻ 4

Do not make every post a giant card.

---

# 17. TOKEN REFERENCES

Support:

$TICKER

and token contract addresses.

When detected:

Show a compact token reference:

Token Name
$TICKER
Price
24h change
Mini chart

Clicking it opens the token page.

This feature must integrate naturally into Space and messaging.

---

# 18. PROFILE

Profile should remain compact.

Example:

┌──────────────────────────────────┐
│ ← Profile ⋮ │
├──────────────────────────────────┤
│ [Avatar] │
│ username │
│ 🟢 Online │
│ │
│ Short bio... │
│ │
│ 128 Friends 2.4K Followers │
├──────────────────────────────────┤
│ Posts | Media | Rooms │
└──────────────────────────────────┘

Actions:

Message
Add Friend
Follow
Mute
Block
Report

---

# 19. NOTIFICATIONS / ALERTS

Notifications should be compact.

Examples:

🟢 Alex sent you a message
👤 Sarah accepted your friend request
💬 John mentioned you
❤️ Someone reacted to your post
🌐 New activity in Indonesia room

Use unread indicators.

Do not make notifications into giant cards.

---

# 20. SEARCH

Unified search must find:

- Users
- Friends
- Chats
- Messages
- Rooms
- Space posts
- Tokens
- Contract addresses

Search should feel like a core messenger feature.

Support:

- Instant suggestions
- Recent searches
- Filters
- Debounced realtime search

---

# 21. WALLET

Wallet is secondary to communication but must use the same Migo design language.

Do NOT make the wallet look like a separate DeFi dashboard.

Use compact:

Balance
Assets
Transactions
Send
Receive
Network
Address
QR

---

# 22. SETTINGS

Settings should be accessible but not dominate the primary experience.

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

Use compact list rows.

---

# 23. DESKTOP LAYOUT

Desktop must become a messenger workspace.

Recommended:

┌──────────────┬────────────────────────┬────────────────────┐
│ Navigation │ Conversations / Chat │ Context │
│ │ │ │
│ Migo │ │ User Profile │
│ │ │ │
│ Friends │ Chat / Room / Space │ Members │
│ Chats │ │ Media │
│ Rooms │ │ Room information │
│ Space │ │ │
│ Alerts │ │ │
│ Search │ │ │
└──────────────┴────────────────────────┴────────────────────┘

IMPORTANT:

The desktop layout must remain compact.

Do not allow enormous empty content areas.

Main content should have sensible maximum widths.

---

# 24. DESKTOP FRIENDS + CHAT

Desktop should support a classic communication workspace.

Example:

┌──────────────┬──────────────────────┬───────────────────┐
│ Friends │ Alex │ Alex │
│ │ 🟢 Online │ 🟢 Online │
│ 🟢 Alex ├──────────────────────┤ │
│ 🟢 Sarah │ │ Profile │
│ 🟡 Mike │ Hello! │ Friends │
│ 🟢 John │ │ Media │
│ │ Hi 👋 │ │
│ ROOMS │ │ │
│ #Indonesia │ │ │
│ #Gaming ├──────────────────────┤ │
│ #Crypto │ Message... ➤ │ │
└──────────────┴──────────────────────┴───────────────────┘

This is much closer to the desired product architecture.

---

# 25. TABLET

Tablet should intelligently switch between:

Navigation + Main

and:

Navigation + Main + Context

depending on width and orientation.

Do not simply stretch the mobile layout.

Do not simply shrink desktop.

---

# 26. MOBILE NAVIGATION

Use a compact bottom navigation for the most important destinations.

Possible:

Home
Chats
Rooms
Space
More

Friends can be directly accessible from Home/Chats or via a compact top tab/navigation structure.

Do not put 10+ destinations into a mobile bottom navigation.

---

# 27. MORE MENU

Secondary features:

Friends
Alerts
Search
Wallet
Profile
Settings
Help

can be accessible through a compact More menu or contextual navigation.

The exact navigation should be optimized after inspecting the existing application.

---

# 28. VISUAL LANGUAGE

Create an original Migo visual identity.

The visual language should be:

- Compact
- Clean
- Friendly
- Slightly nostalgic in simplicity
- Modern
- Lightweight
- Information-dense
- Social
- Realtime

Avoid excessive:

- Glassmorphism
- Gradients
- Huge rounded cards
- Huge shadows
- Decorative illustrations
- Empty whitespace
- Oversized typography

Use subtle:

- Borders
- Dividers
- Surface elevation
- Rounded corners
- Status colors
- Selected states

---

# 29. INFORMATION DENSITY

This is extremely important.

The interface should allow many useful items to fit on screen.

Prefer:

COMPACT LIST

over:

LARGE CARD

Prefer:

USERNAME
status
last message

over:

Huge avatar
Huge username
Large card
Large empty area

The UI should be dense without becoming visually chaotic.

---

# 30. DESIGN TOKENS

Create centralized design tokens.

Define:

- Colors
- Typography
- Font sizes
- Font weights
- Line heights
- Spacing
- Border radius
- Borders
- Shadows
- Icon sizes
- Touch targets
- Animation durations
- Z-index

Do not scatter arbitrary CSS values throughout the project.

---

# 31. TYPOGRAPHY

Use system-friendly typography.

Prioritize:

Username
Message
Status
Timestamp
Metadata

Avoid giant headings.

Page titles should be compact.

---

# 32. ICONS

Use ONE consistent icon family.

Icons should be:

- Simple
- Lightweight
- Consistent
- Recognizable

Typical icon visual size:

20–24px

Touch target:

minimum 44×44 logical pixels.

---

# 33. LIGHT + DARK MODE

Support:

- Light
- Dark
- System

Dark mode must be intentionally designed.

Do not simply invert colors.

---

# 34. TOUCH UX

Support:

- Tap
- Long press
- Swipe
- Pull to refresh
- Swipe back
- Context menus
- Bottom sheets

Long press:

User → user actions

Message → message actions

Room → room actions

---

# 35. DESKTOP UX

Support:

- Mouse
- Hover
- Right click
- Keyboard shortcuts
- Drag where appropriate
- Resizable panels where useful

But retain the same Migo visual identity.

---

# 36. REALTIME UX

Realtime events:

- Online status
- Typing
- New messages
- Read status
- Reactions
- Room messages
- Notifications

must update immediately.

Avoid full-page rerenders.

---

# 37. PERFORMANCE

Migo should remain lightweight.

Implement:

- Lazy loading
- Virtualized message lists
- Virtualized room lists
- Pagination
- Image optimization
- Thumbnail loading
- Efficient WebSocket updates
- Minimal rerenders
- Caching
- Code splitting

Large rooms must remain performant.

Large chat histories must remain performant.

---

# 38. OFFLINE / RECONNECT

Show subtle states:

● Connected
○ Connecting
⚠ Reconnecting
× Offline

Do not use disruptive modal dialogs for temporary network problems.

---

# 39. MOBILE WEB

Mobile Web must feel like an actual application.

Support:

- PWA
- Safe areas
- Sticky headers
- Sticky composer
- Bottom navigation
- Keyboard-aware layouts
- Scroll restoration
- Offline shell

Never make it look like a desktop website squeezed into a phone.

---

# 40. ANDROID + IOS

Respect platform conventions:

Android:

- Back gesture
- Edge-to-edge
- Keyboard behavior
- Notifications
- Deep links

iOS:

- Safe areas
- Swipe back
- Keyboard avoidance
- Dynamic Island/notch
- Home indicator
- Deep links

But maintain the same Migo design system.

---

# 41. RESPONSIVE BREAKPOINTS

Test:

320
360
375
390
412
430
480
600
768
820
1024
1280
1440
1920+

The interface must never:

- Overflow horizontally
- Clip text
- Overlap elements
- Hide important controls
- Break the composer
- Break navigation

---

# 42. COMPONENT ARCHITECTURE

Create reusable components:

AppShell
MessengerShell
Header
Navigation
BottomNavigation
Sidebar
ContactList
ContactRow
PresenceIndicator
ConversationList
ConversationRow
ChatView
ChatHeader
MessageList
MessageBubble
MessageComposer
RoomList
RoomRow
RoomView
RoomHeader
RoomMemberList
SpaceFeed
SpacePost
TokenReference
ProfileView
NotificationList
NotificationRow
SearchView
WalletView
SettingsView
ContextMenu
BottomSheet
Dialog
Toast
Skeleton
EmptyState
ErrorState

All components must be responsive.

---

# 43. EXISTING APPLICATION

Before modifying anything:

INSPECT THE EXISTING APPLICATION FIRST.

Inspect:

- Framework
- Routing
- Components
- State management
- Authentication
- API
- WebSocket/realtime architecture
- Chat implementation
- Room implementation
- Space
- Wallet
- Responsive behavior
- Existing design tokens

Reuse working functionality.

Do NOT unnecessarily rebuild backend functionality.

Do NOT replace working infrastructure merely to change the UI.

Refactor only when necessary.

---

# 44. IMPLEMENTATION ORDER

Phase 1:
Inspect existing architecture.

Phase 2:
Create Migo Design System.

Phase 3:
Create MessengerShell/AppShell.

Phase 4:
Create responsive navigation.

Phase 5:
Implement:

Friends
Chats
Private Chat
Rooms
Room Chat

Phase 6:

Space
Profile
Notifications
Search

Phase 7:

Wallet
Settings

Phase 8:

Dark mode
Accessibility
PWA
Performance
Offline/reconnect

Phase 9:

Full responsive QA.

---

# 45. VISUAL QA REQUIREMENT

After implementation, inspect screenshots of the actual application at:

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

Compare the result against the reference material.

Ask:

"Does this look like a messenger?"

If the answer is no, redesign it.

Ask:

"Is the first thing users see people, conversations, presence, rooms and activity?"

If no, redesign it.

Ask:

"Does this look like a SaaS dashboard?"

If yes, redesign it.

Ask:

"Is there excessive empty space?"

If yes, redesign it.

Ask:

"Are Friends, Chats and Rooms visually important?"

If no, redesign it.

---

# 46. HARD PROHIBITIONS

DO NOT:

- Build a generic SaaS sidebar
- Build a dashboard-first interface
- Put huge empty areas on the main screen
- Use oversized cards
- Use huge page headers
- Make navigation dominate the screen
- Hide friends and conversations behind secondary menus
- Make rooms feel like an afterthought
- Make chat look like a dashboard widget
- Use random UI patterns per page
- Create separate design languages per platform
- Copy another product's branding

---

# 47. FINAL DESIGN TEST

When the application is opened for the first time, a user should immediately understand:

"This is a social messenger."

They should immediately be able to see:

- Who is online
- Recent conversations
- Friends
- Rooms
- Social activity

The application should feel alive.

It should not feel like an empty enterprise dashboard.

---

# FINAL OBJECTIVE

Create a modern Migo that combines:

REALTIME MESSENGER +
CONTACT / FRIEND SYSTEM +
PUBLIC CHAT ROOMS +
SOCIAL SPACE +
PROFILE +
DISCOVERY +
WALLET

with one unified cross-platform experience.

The design must preserve:

- compact information density
- fast navigation
- contact-first interaction
- visible presence
- conversation-first UX
- room-centric communication
- contextual menus
- efficient small-screen layouts

while providing:

- modern typography
- modern accessibility
- responsive layouts
- dark mode
- touch interaction
- desktop support
- tablet support
- Android support
- iOS support
- mobile web support
- high performance

The final product must feel like:

"A modern, original, highly polished messenger that happens to work beautifully everywhere."

NOT:

"A SaaS dashboard that happens to contain chat."
