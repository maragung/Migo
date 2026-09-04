/**
 * The store's packs: what the server's catalogue prices, as this app's art.
 *
 * The catalogue on the server is a price list — SKU, coins — and the SKU's slug names a pack.
 * This file is the other half of that contract (the same one the web client's
 * `lib/store/packs.ts` holds): the emoji a pack's slug stands for, held client-side because art
 * is the client's to ship, not the server's to store. A slug priced on the server with no pack
 * here is a pack nobody can render; the two lists change together.
 *
 * The glyphs are Unicode at sticker scale — no binary art to ship or fetch, and the conversation
 * they ride in is E2EE either way: a sticker goes out as ordinary message text.
 */

/** One purchasable pack. */
export interface StorePack {
  /** The full catalogue code, as the server's catalogue prices it. */
  sku: string;
  /** The shelf the pack sits on. */
  kind: 'emoticon' | 'sticker';
  /** The display name. */
  name: string;
  /** What the pack holds, in picker order. */
  items: string[];
}

/** Every pack this client can render, in catalogue order (mirrors the server's `STORE_PACKS`). */
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

/** The store's shelves, as the nav names them. */
export interface Shelf {
  /** The path segment: `/store/<slug>`. */
  slug: string;
  /** The nav label. */
  label: string;
  /** Which packs sit on this shelf. */
  packs: ReadonlyArray<StorePack>;
}

/** The shelves, in nav order. Stickers are this build's priced packs; the other shelves seed from the same list. */
export const SHELVES: ReadonlyArray<Shelf> = [
  {
    slug: 'emoticons-pack',
    label: 'Emoticon Packs',
    packs: STORE_PACKS,
  },
  {
    slug: 'stickers',
    label: 'Stickers',
    packs: STORE_PACKS,
  },
  {
    slug: 'gift',
    label: 'Gifts',
    packs: [],
  },
  {
    slug: 'avatar',
    label: 'Avatars',
    packs: [],
  },
];

/** The pack a SKU names, when this client can render it. */
export function packOfSku(sku: string): StorePack | null {
  return STORE_PACKS.find((pack) => pack.sku === sku) ?? null;
}
