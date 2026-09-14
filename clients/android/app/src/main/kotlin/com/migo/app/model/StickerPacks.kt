package com.migo.app.model

/**
 * The store's packs: what the server's catalogue prices, as this client's art.
 *
 * The catalogue on the server is a price list -- SKU, coins -- and the SKU's slug names a pack.
 * This file is the other half of that contract: the emoji (or sticker glyphs) a pack's slug stands
 * for, held client-side because art is the client's to ship and render, not the server's to store.
 * A slug priced on the server with no pack here is a pack nobody can render; the two lists change
 * together. A port of the web client's own `lib/store/packs.ts`, glyph for glyph, so a pack bought
 * on one client renders on the other.
 *
 * Emoticons are Unicode the composer can insert as text; stickers are larger one-shot images the
 * picker renders inline (Unicode glyphs at sticker scale -- no binary art to ship, sign, or fetch,
 * and the conversation they ride in is E2EE either way: the glyphs go out as ordinary message
 * text).
 */

/** The shelf a pack sits on. */
enum class PackKind {
    Emoticon,
    Sticker,
}

/** One purchasable pack. */
data class StorePack(
    /** The full catalogue code, as the server's catalogue prices it. */
    val sku: String,
    /** The shelf the pack sits on. */
    val kind: PackKind,
    /** The display name. */
    val name: String,
    /** What the pack holds, in picker order: emoticon strings or sticker glyphs. */
    val items: List<String>,
)

/** Every pack this client can render. */
val STORE_PACKS: List<StorePack> = listOf(
    StorePack(
        sku = "sticker.frog_set",
        kind = PackKind.Sticker,
        name = "Frog Pack",
        items = listOf("🐸", "🐸☕", "🐸💤", "🐸❗", "🐸🤝", "🐸🎯", "🐸💚", "🐸🎉"),
    ),
    StorePack(
        sku = "sticker.cat_set",
        kind = PackKind.Sticker,
        name = "Cat Pack",
        items = listOf("🐱", "😺", "😹", "😻", "😼", "🙀", "😿", "😽"),
    ),
    StorePack(
        sku = "sticker.panda_set",
        kind = PackKind.Sticker,
        name = "Panda Pack",
        items = listOf("🐼", "🐼🍜", "🐼💤", "🐼🎋", "🐼❤️", "🐼🎲", "🐼🎊", "🐼🌟"),
    ),
    StorePack(
        sku = "sticker.party_set",
        kind = PackKind.Sticker,
        name = "Party Pack",
        items = listOf("🎉", "🥳", "🎈", "🎊", "🍾", "🎂", "🪩", "🎁"),
    ),
    StorePack(
        sku = "sticker.love_set",
        kind = PackKind.Sticker,
        name = "Love Pack",
        items = listOf("❤️", "😍", "😘", "💐", "🌹", "💘", "💞", "💌"),
    ),
    StorePack(
        sku = "sticker.work_set",
        kind = PackKind.Sticker,
        name = "Work Pack",
        items = listOf("💻", "☕", "📈", "📌", "✅", "⏰", "📝", "🎯"),
    ),
    StorePack(
        sku = "sticker.summer_set",
        kind = PackKind.Sticker,
        name = "Summer Pack",
        items = listOf("🏖️", "🌴", "🍉", "🌞", "😎", "🏊", "⛵", "🍦"),
    ),
    StorePack(
        sku = "sticker.spooky_set",
        kind = PackKind.Sticker,
        name = "Spooky Pack",
        items = listOf("👻", "🎃", "🕷️", "🦇", "💀", "🕸️", "🧙", "🌑"),
    ),
    StorePack(
        sku = "sticker.newyear_set",
        kind = PackKind.Sticker,
        name = "New Year Pack",
        items = listOf("🎊", "🎆", "🎇", "🥂", "⏳", "🗓️", "🌟", "🎈"),
    ),
)

/** The free baseline every account can use: the picker's always-present Emoticons set. */
val FREE_EMOTICONS: List<String> = listOf(
    "😀", "😂", "🙂", "😉", "😍", "🤔", "😴", "😎",
    "😢", "😭", "😡", "🤯", "🥺", "😱", "🤗", "🤩",
    "👍", "👎", "🙏", "👏", "💪", "🤝", "✌️", "🫶",
    "❤️", "🔥", "✨", "🎉", "💯", "✅", "❌", "⚡",
)

/**
 * The emoticon items the account's owned packs add to the picker.
 *
 * `owned` is the SKU set from the account's entitlements; a pack this client cannot render is
 * skipped rather than shown as a name with nothing to tap.
 */
fun ownedEmoticons(owned: Set<String>): List<String> = buildList {
    for (pack in STORE_PACKS) {
        if (pack.kind == PackKind.Emoticon && pack.sku in owned) {
            addAll(pack.items)
        }
    }
}

/** The sticker packs the account owns and this client can render, in catalogue order. */
fun ownedStickerPacks(owned: Set<String>): List<StorePack> =
    STORE_PACKS.filter { it.kind == PackKind.Sticker && it.sku in owned }
