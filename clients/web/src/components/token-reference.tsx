'use client';

/**
 * The $MIG token reference: Migo's own coin, recognised in message text.
 *
 * Migo's currency is $MIG — the one ticker this build knows — and a mention of it in a message
 * is a live reference to the wallet, not decoration. This component splits a message's text on
 * the ticker (word-bounded, case-insensitive) and renders each match as a chip that opens the
 * Wallet section; everything around the match renders exactly as it did before, so the message
 * stays the author's words with the reference made tappable.
 *
 * The split is deliberate about what counts: `$MIG` and `$mig` are the ticker; `$MIGOCOIN` is a
 * longer word the boundary excludes, and a lone `$` is not a reference at all. The chip is a
 * button in the text's inline flow — small, keyboard-reachable, and labelled for what it does.
 */

import type { ReactNode } from 'react';

/** The ticker this build recognises, as a word-bounded case-insensitive match. */
const TICKER_PATTERN = /\$MIG\b/gi;

/**
 * The text with every $MIG reference rendered as a chip.
 *
 * @param text The message's plain text, as the wire delivered it.
 * @param onOpenWallet Called when a rendered chip is clicked — the shell's way into the Wallet.
 */
export function TokenText({
  text,
  onOpenWallet,
}: {
  text: string;
  onOpenWallet: () => void;
}): ReactNode {
  const parts: ReactNode[] = [];
  let cursor = 0;
  let match: RegExpExecArray | null;
  // A fresh regex per call: a shared global regex carries lastIndex between renders.
  const pattern = new RegExp(TICKER_PATTERN.source, TICKER_PATTERN.flags);
  let index = 0;

  while ((match = pattern.exec(text)) !== null) {
    if (match.index > cursor) {
      parts.push(text.slice(cursor, match.index));
    }
    parts.push(<TokenChip key={`token-${match.index}-${index}`} onOpenWallet={onOpenWallet} />);
    index += 1;
    cursor = match.index + match[0].length;
  }
  if (cursor < text.length) {
    parts.push(text.slice(cursor));
  }
  return parts.length === 1 && typeof parts[0] === 'string' ? text : parts;
}

/** One rendered reference: the ticker as a chip, opening the wallet. */
function TokenChip({ onOpenWallet }: { onOpenWallet: () => void }): ReactNode {
  return (
    <button
      type="button"
      className="token-ref"
      title="Open your wallet"
      aria-label="$MIG — open your wallet"
      onClick={(event) => {
        // A chip inside a message must not also click the message's own row.
        event.stopPropagation();
        onOpenWallet();
      }}
    >
      $MIG
    </button>
  );
}
