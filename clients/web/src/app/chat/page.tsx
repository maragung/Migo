import type { ReactNode } from 'react';

/** Shown in the thread area when no conversation is selected. */
export default function ChatIndexPage(): ReactNode {
  return (
    <div className="empty-thread">
      <div className="empty-thread-inner">
        <div className="emoji">🔒</div>
        <h2>Select a conversation</h2>
        <p>
          Your messages are end-to-end encrypted. Pick a conversation on the left, or start a new
          one.
        </p>
      </div>
    </div>
  );
}
