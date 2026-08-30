'use client';

/**
 * The inline gift picker for the composer: a small catalogue and a recipient, nothing else.
 *
 * A gift is a spend, so the picker states the price on every card before the send — the same
 * rule the Gifts tab's picker follows, kept in the composer so the flow never has to leave the
 * conversation. The catalogue is the caller's concern (the top of the server's list, fetched
 * once and cached by the chat window); recipients are the conversation's own members, because a
 * gift sent from a thread names someone in it. A direct conversation has exactly one candidate,
 * so its recipient is pre-chosen and the picker is just the cards.
 */

import type { ReactNode } from 'react';

import type { GiftListing, Id } from '@migo/sdk';

import { Avatar } from './avatar.js';

/** One candidate recipient: avatar and name, clickable to choose. */
export function RecipientOption({
  id,
  name,
  selected,
  onPick,
}: {
  id: Id;
  name: string;
  selected: boolean;
  onPick: (id: Id) => void;
}): ReactNode {
  return (
    <button
      type="button"
      className={`person-row recipient-option ${selected ? 'selected' : ''}`}
      onClick={() => onPick(id)}
      aria-pressed={selected}
      aria-label={`Choose ${name} as the recipient`}
    >
      <Avatar name={name} id={id} size={28} />
      <div className="person-main">
        <span className="person-name">{name}</span>
      </div>
      {selected ? <span className="tag tag-current">Recipient</span> : null}
    </button>
  );
}

/** The picker: recipients first (when there is a choice), then the catalogue. */
export function GiftPicker({
  gifts,
  recipients,
  selectedRecipient,
  onSelectRecipient,
  onSend,
  onClose,
  busy,
}: {
  /** The catalogue slice the caller chose to offer. */
  gifts: GiftListing[];
  /** The conversation's members as candidate recipients, excluding ourselves. */
  recipients: ReadonlyArray<{ id: Id; name: string }>;
  /** The currently chosen recipient, when one is. */
  selectedRecipient: Id | null;
  onSelectRecipient: (id: Id) => void;
  onSend: (gift: GiftListing, recipient: Id) => void;
  onClose: () => void;
  busy: boolean;
}): ReactNode {
  const [only] = recipients;
  const single = recipients.length === 1 && only !== undefined ? only : null;
  const target = single !== null ? single.id : selectedRecipient;
  const singleName = single !== null ? single.name : null;
  return (
    <div className="gift-picker" role="dialog" aria-label="Send a gift in this chat">
      <div className="panel-head">
        <h2 className="panel-heading">Send a gift</h2>
        <button type="button" className="icon-btn" onClick={onClose} aria-label="Close gift picker">
          ✕
        </button>
      </div>

      {singleName !== null ? (
        <p className="hint">To {singleName}</p>
      ) : recipients.length > 0 ? (
        <div className="gift-recipients">
          {recipients.map((person) => (
            <RecipientOption
              key={person.id}
              id={person.id}
              name={person.name}
              selected={target === person.id}
              onPick={onSelectRecipient}
            />
          ))}
        </div>
      ) : (
        <p className="hint">No one here to send a gift to yet.</p>
      )}

      <div className="gift-grid">
        {gifts.map((gift) => (
          <div key={gift.sku} className="gift-card">
            <div className="gift-name">{gift.name}</div>
            <div className="gift-price">{gift.price} coins</div>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || target === null}
              onClick={() => target !== null && onSend(gift, target)}
              aria-label={`Send ${gift.name} for ${gift.price} coins`}
            >
              Send
            </button>
          </div>
        ))}
      </div>
      {gifts.length === 0 ? <p className="muted">The gift shop is empty on this server.</p> : null}
    </div>
  );
}
