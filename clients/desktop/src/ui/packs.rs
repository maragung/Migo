//! The composer's packs: what the server's catalogue prices, as this client's art.
//!
//! The catalogue on the server is a price list — SKU, coins — and the SKU's slug names a pack.
//! This module is the other half of that contract: the glyphs a pack's slug stands for, held
//! client-side because art is the client's to ship and render, not the server's to store. A
//! slug priced on the server with no pack here is a pack nobody can render; the two lists
//! change together, and the web client ships the same table (`packs.ts`) so a pack bought on
//! one client renders on the other.
//!
//! Emoticons are Unicode the composer inserts as text; stickers are larger one-shot glyphs
//! the picker renders at sticker scale — still Unicode, still text on the wire, because the
//! conversation they ride in is end-to-end encrypted either way and there is no binary art to
//! fetch. The *size* they render at downstream is the receiver's presentation choice.

/// One purchasable pack.
pub struct StorePack {
    /// The full catalogue code, as the server's catalogue prices it.
    pub sku: &'static str,
    /// The shelf the pack sits on.
    pub kind: PackKind,
    /// The display name.
    pub name: &'static str,
    /// What the pack holds, in picker order.
    pub items: &'static [&'static str],
}

/// The two shelves a pack can sit on, as the picker's two tabs divide them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PackKind {
    Emoticon,
    Sticker,
}

/// Every pack this client can render — the web client's own table, kept in step so a pack
/// bought anywhere renders everywhere.
pub const STORE_PACKS: &[StorePack] = &[
    StorePack {
        sku: "sticker.frog_set",
        kind: PackKind::Sticker,
        name: "Frog Pack",
        items: &["🐸", "🐸☕", "🐸💤", "🐸❗", "🐸🤝", "🐸🎯", "🐸💚", "🐸🎉"],
    },
    StorePack {
        sku: "sticker.cat_set",
        kind: PackKind::Sticker,
        name: "Cat Pack",
        items: &["🐱", "😺", "😹", "😻", "😼", "🙀", "😿", "😽"],
    },
    StorePack {
        sku: "sticker.panda_set",
        kind: PackKind::Sticker,
        name: "Panda Pack",
        items: &["🐼", "🐼🍜", "🐼💤", "🐼🎋", "🐼❤️", "🐼🎲", "🐼🎊", "🐼🌟"],
    },
    StorePack {
        sku: "sticker.party_set",
        kind: PackKind::Sticker,
        name: "Party Pack",
        items: &["🎉", "🥳", "🎈", "🎊", "🍾", "🎂", "🪩", "🎁"],
    },
    StorePack {
        sku: "sticker.love_set",
        kind: PackKind::Sticker,
        name: "Love Pack",
        items: &["❤️", "😍", "😘", "💐", "🌹", "💘", "💞", "💌"],
    },
    StorePack {
        sku: "sticker.work_set",
        kind: PackKind::Sticker,
        name: "Work Pack",
        items: &["💻", "☕", "📈", "📌", "✅", "⏰", "📝", "🎯"],
    },
    StorePack {
        sku: "sticker.summer_set",
        kind: PackKind::Sticker,
        name: "Summer Pack",
        items: &["🏖️", "🌴", "🍉", "🌞", "😎", "🏊", "⛵", "🍦"],
    },
    StorePack {
        sku: "sticker.spooky_set",
        kind: PackKind::Sticker,
        name: "Spooky Pack",
        items: &["👻", "🎃", "🕷️", "🦇", "💀", "🕸️", "🧙", "🌑"],
    },
    StorePack {
        sku: "sticker.newyear_set",
        kind: PackKind::Sticker,
        name: "New Year Pack",
        items: &["🎊", "🎆", "🎇", "🥂", "⏳", "🗓️", "🌟", "🎈"],
    },
];

/// The free baseline every account can use: the picker's always-present Emoticons set, the
/// same glyphs the web picker offers before any pack is owned.
pub const FREE_EMOTICONS: &[&str] = &[
    "😀", "😂", "🙂", "😉", "😍", "🤔", "😴", "😎", "😢", "😭", "😡", "🤯", "🥺", "😱", "🤗", "🤩",
    "👍", "👎", "🙏", "👏", "💪", "🤝", "✌️", "🫶", "❤️", "🔥", "✨", "🎉", "💯", "✅", "❌", "⚡",
];

/// The emoticon glyphs the account's owned packs add to the picker's first tab.
///
/// `owned` is the SKU set from the account's entitlements; a pack this client cannot render
/// is skipped rather than shown as a name with nothing to tap.
pub fn owned_emoticons(owned: &std::collections::HashSet<String>) -> Vec<&'static str> {
    STORE_PACKS
        .iter()
        .filter(|pack| pack.kind == PackKind::Emoticon && owned.contains(pack.sku))
        .flat_map(|pack| pack.items.iter().copied())
        .collect()
}

/// The sticker packs the account owns and this client can render, in catalogue order — the
/// picker's second tab, grouped by pack because the pack is what was bought and the pack is
/// what the eye scans.
pub fn owned_sticker_packs(owned: &std::collections::HashSet<String>) -> Vec<&'static StorePack> {
    STORE_PACKS
        .iter()
        .filter(|pack| pack.kind == PackKind::Sticker && owned.contains(pack.sku))
        .collect()
}
