/**
 * The store's packs: what the server's catalogue prices, as the client's art.
 *
 * The catalogue on the server is a price list — SKU, coins — and the SKU's slug names a pack.
 * This file is the other half of that contract: the emoji (or sticker glyphs) a pack's slug
 * stands for, held client-side because art is the client's to ship and render, not the
 * server's to store. A slug priced on the server with no pack here is a pack nobody can
 * render; the two lists change together.
 *
 * Emoticons are Unicode the composer can insert as text; stickers are larger one-shot images
 * the picker renders inline (Unicode glyphs at sticker scale — no binary art to ship, sign,
 * or fetch, and the conversation they ride in is E2EE either way: the glyphs go out as
 * ordinary message text).
 */

/** One purchasable pack. */
export interface StorePack {
  /** The full catalogue code, as the server's catalogue prices it. */
  sku: string;
  /** The shelf the pack sits on. */
  kind: 'emoticon' | 'sticker';
  /** The display name. */
  name: string;
  /** What the pack holds, in picker order: emoticon strings or sticker glyphs. */
  items: string[];
}

/** Every pack this client can render. */
export const STORE_PACKS: ReadonlyArray<StorePack> = [
  {
    sku: 'sticker.frog_set',
    kind: 'sticker',
    name: 'Frog Pack',
    items: ['🐸', '🐸☕', '🐸💤', '🐸❗', '🐸🤝', '🐸🎯', '🐸💚', '🐸🎉'],
  },
  {
    sku: 'sticker.cat_set',
    kind: 'sticker',
    name: 'Cat Pack',
    items: ['🐱', '😺', '😹', '😻', '😼', '🙀', '😿', '😽'],
  },
  {
    sku: 'sticker.panda_set',
    kind: 'sticker',
    name: 'Panda Pack',
    items: ['🐼', '🐼🍜', '🐼💤', '🐼🎋', '🐼❤️', '🐼🎲', '🐼🎊', '🐼🌟'],
  },
  {
    sku: 'sticker.party_set',
    kind: 'sticker',
    name: 'Party Pack',
    items: ['🎉', '🥳', '🎈', '🎊', '🍾', '🎂', '🪩', '🎁'],
  },
  {
    sku: 'sticker.love_set',
    kind: 'sticker',
    name: 'Love Pack',
    items: ['❤️', '😍', '😘', '💐', '🌹', '💘', '💞', '💌'],
  },
  {
    sku: 'sticker.work_set',
    kind: 'sticker',
    name: 'Work Pack',
    items: ['💻', '☕', '📈', '📌', '✅', '⏰', '📝', '🎯'],
  },
  {
    sku: 'sticker.summer_set',
    kind: 'sticker',
    name: 'Summer Pack',
    items: ['🏖️', '🌴', '🍉', '🌞', '😎', '🏊', '⛵', '🍦'],
  },
  {
    sku: 'sticker.spooky_set',
    kind: 'sticker',
    name: 'Spooky Pack',
    items: ['👻', '🎃', '🕷️', '🦇', '💀', '🕸️', '🧙', '🌑'],
  },
  {
    sku: 'sticker.newyear_set',
    kind: 'sticker',
    name: 'New Year Pack',
    items: ['🎊', '🎆', '🎇', '🥂', '⏳', '🗓️', '🌟', '🎈'],
  },
];

/** The free baseline every account can use: the picker's always-present Emoticons set. */
export const FREE_EMOTICONS: readonly string[] = [
  '😀',
  '😂',
  '🙂',
  '😉',
  '😍',
  '🤔',
  '😴',
  '😎',
  '😢',
  '😭',
  '😡',
  '🤯',
  '🥺',
  '😱',
  '🤗',
  '🤩',
  '👍',
  '👎',
  '🙏',
  '👏',
  '💪',
  '🤝',
  '✌️',
  '🫶',
  '❤️',
  '🔥',
  '✨',
  '🎉',
  '💯',
  '✅',
  '❌',
  '⚡',
];

/** The pack a SKU names, when this client can render it. */
export function packOfSku(sku: string): StorePack | null {
  return STORE_PACKS.find((pack) => pack.sku === sku) ?? null;
}

/**
 * The emoticon items the account's owned packs add to the picker.
 *
 * `owned` is the SKU set from the account's entitlements; a pack this client cannot render is
 * skipped rather than shown as a name with nothing to tap.
 */
export function ownedEmoticons(owned: ReadonlySet<string>): string[] {
  const items: string[] = [];
  for (const pack of STORE_PACKS) {
    if (pack.kind === 'emoticon' && owned.has(pack.sku)) {
      items.push(...pack.items);
    }
  }
  return items;
}

/** The sticker packs the account owns and this client can render, in catalogue order. */
export function ownedStickerPacks(owned: ReadonlySet<string>): StorePack[] {
  return STORE_PACKS.filter((pack) => pack.kind === 'sticker' && owned.has(pack.sku));
}
