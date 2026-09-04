'use client';

/**
 * The composer's emoticon and sticker picker: two tabs, one free baseline, every pack owned.
 *
 * The Emoticons tab is the free set every account has, plus the emoticon items of purchased
 * packs. The Stickers tab is the sticker packs the account owns, grouped by pack with a header
 * — a sticker is chosen from its set, not from a merged wall, because the pack is what was
 * bought and the pack is what the eye scans.
 *
 * Everything in either tab inserts as text: the glyphs are Unicode, the conversation is E2EE,
 * and a sticker rides out as ordinary message text the way an emoticon does — the *size* it
 * renders at downstream is the receiver's presentation choice. A pack in the catalogue but not
 * in this client's `packs.ts` (art the client does not ship) simply does not appear here; the
 * store's page is where a mismatch between owned and renderable would surface.
 */

import { useState } from 'react';
import type { ReactNode } from 'react';

import { FREE_EMOTICONS, ownedStickerPacks } from '@/lib/store/packs.js';
import { ownedEmoticons } from '@/lib/store/packs.js';

/** The picker's two tabs, in the order the reference draws them. */
type PickerTab = 'emoticons' | 'stickers';

/**
 * @param owned The account's owned-SKU set (`null` while the read is in flight — the picker
 *   waits rather than showing a free-only set that would read as "you own nothing").
 * @param onInsert Emits the chosen glyph into the composer's text.
 * @param onClose Closes the picker.
 */
export function EmoticonPicker({
  owned,
  onInsert,
  onClose,
}: {
  owned: ReadonlySet<string> | null;
  onInsert: (text: string) => void;
  onClose: () => void;
}): ReactNode {
  const [tab, setTab] = useState<PickerTab>('emoticons');
  const emoticons = owned === null ? [] : FREE_EMOTICONS.concat(ownedEmoticons(owned));
  const stickerPacks = owned === null ? [] : ownedStickerPacks(owned);

  return (
    <div className="emoticon-picker" role="dialog" aria-label="Emoticons and stickers">
      <div className="panel-head">
        <div className="chip-row" role="tablist" aria-label="Picker tab">
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'emoticons'}
            className={`chip ${tab === 'emoticons' ? 'chip-active' : ''}`}
            onClick={() => setTab('emoticons')}
          >
            Emoticons
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'stickers'}
            className={`chip ${tab === 'stickers' ? 'chip-active' : ''}`}
            onClick={() => setTab('stickers')}
          >
            Stickers
          </button>
        </div>
        <button type="button" className="icon-btn" onClick={onClose} aria-label="Close picker">
          ✕
        </button>
      </div>

      {tab === 'emoticons' ? (
        owned === null ? (
          <p className="hint">Reading your packs…</p>
        ) : (
          <div className="emoticon-grid" role="listbox" aria-label="Emoticons">
            {emoticons.map((glyph) => (
              <button
                key={glyph}
                type="button"
                role="option"
                aria-selected={false}
                className="emoticon-cell"
                onClick={() => onInsert(glyph)}
                aria-label={`Insert ${glyph}`}
              >
                {glyph}
              </button>
            ))}
          </div>
        )
      ) : owned === null ? (
        <p className="hint">Reading your packs…</p>
      ) : stickerPacks.length === 0 ? (
        <div className="emoticon-empty">
          <p className="hint">You do not own any sticker packs yet.</p>
          <p className="muted">The store sells them — Profile menu → Store.</p>
        </div>
      ) : (
        <div className="sticker-groups">
          {stickerPacks.map((pack) => (
            <div key={pack.sku} className="sticker-group">
              <p className="sticker-group-name">{pack.name}</p>
              <div className="emoticon-grid sticker-grid" role="listbox" aria-label={pack.name}>
                {pack.items.map((glyph) => (
                  <button
                    key={glyph}
                    type="button"
                    role="option"
                    aria-selected={false}
                    className="emoticon-cell sticker-cell"
                    onClick={() => onInsert(glyph)}
                    aria-label={`Send ${glyph}`}
                  >
                    {glyph}
                  </button>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
