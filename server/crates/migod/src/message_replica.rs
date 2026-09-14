//! Seating a message row that crossed the mesh, so a node that watches a
//! conversation also *holds* it (section 170's message-row tier).
//!
//! Before this tier, a message lived as a row on exactly one node — the node
//! whose session accepted the send, because that node's append is the store
//! write — and every other node in the conversation's audience only ever saw
//! the live push: the hub copy the mesh carried, delivered to the sessions
//! that happened to be connected and to nobody else. A member on another
//! node who reconnected, or opened the conversation on a device that had
//! never seen it, asked for history and got an empty page, because sync
//! reads the local store and the local store never held the rows.
//!
//! The tier works at the ingest boundary, which is the one place every copy
//! of a conversation event already passes through on every node:
//! [`IngestRouter::route_conversation_event`](crate::mesh::IngestRouter::route_conversation_event)
//! for the conversations a room does not own, and `route_room_event` for the
//! chat a room *does* own — the same inner frame in both envelopes, a
//! [`MessageEvent`], which is why both routes
//! call the one [`seat`] here rather than each growing its own half of a
//! mechanism. When the inner frame is a message, the row is written to this
//! node's store through [`replicate_message`](migo_store::traits::MessagingStore::replicate_message),
//! whose contract is the honest shape of the whole tier:
//!
//! * idempotent by `(conversation_id, message_id)`, because the mesh is
//!   at-least-once (section 153) and a redelivery is a no-op, not a
//!   duplicate row;
//! * never over a row this node already holds, the same posture every other
//!   replication half keeps — a peer cannot overwrite local truth, and the
//!   origin node seats its own sends through its own request path, never
//!   through ingest;
//! * sequenced verbatim, because the sending node is the sequencer of its
//!   own sends (section 170), and the home node is fan-out authority, not a
//!   renumberer;
//! * monotonic on `last_seq`, so the receiving node's sync answer about
//!   truncation stays truthful even when two nodes' numbering orders differ;
//! * an edit or a tombstone carrier applied to a row already held, because
//!   the wire's one event shape carries all three facts and the differences
//!   between them belong to the store, not to the sender.
//!
//! # What this tier cannot do
//!
//! The honest limits, recorded rather than papered over:
//!
//! * **Expiry.** [`MessageEvent`] carries no
//!   `expires_at`, so a replica of a disappearing message never learns when
//!   it was due to vanish; the replica row is seated without an expiry and
//!   stays until a tombstone or the operator removes it. That is a wire gap,
//!   not a store decision, and closing it needs a protocol change this
//!   tier's frozen envelope cannot make.
//! * **The hole before the watch.** A node that starts watching a
//!   conversation mid-history seats everything from that moment forward;
//!   the messages before it are on nodes that hold them, and backfill needs
//!   a message-row query the wire does not have. Sync on the new watcher
//!   reports the transcript as truncated, which is the truthful answer.
//! * **Receipt cursors.** Delivered and read positions are per-member state
//!   the message event does not carry; replicating them is a separate tail.
//!
//! The seat itself never fails the ingest that carries it: a store fault
//! costs this node the *row* while the hub copy still reaches every live
//! session, and the sender's redelivery may still make the row good, so the
//! failure is logged and swallowed — the push and the row are two halves of
//! one delivery, and the push half has already succeeded by the time this
//! runs.

use migo_core::Timestamp;
use migo_protocol::MessageEvent;
use migo_store::model::ReplicaMessage;
// No `MessagingStore` import here: `SharedStore` is a `dyn Store` trait object,
// whose supertrait methods resolve on the object itself, so the trait named in
// the doc link above never needs to be in this file's scope.
use migo_store::SharedStore;

/// Seats one message event's row in this node's store.
///
/// Called from the ingest path for every [`MessageEvent`] that crossed the
/// mesh — in either envelope — with `now` the receiving node's clock, which
/// is the only timestamp available for a tombstone: the wire's deletion flag
/// carries no deletion time of its own, so the row records when *this node*
/// learned the message was gone.
///
/// Returns whether the store wrote a row; the caller logs at debug and moves
/// on either way, because a seat that wrote nothing is usually the
/// redelivery the at-least-once mesh owes or the origin's own copy arriving
/// back, and neither is a condition an operator needs on a console.
pub async fn seat(store: &SharedStore, event: &MessageEvent, now: Timestamp) {
    let Some(replica) = replica_of(event, now) else {
        // A seq the store cannot number is a seq no origin can have assigned
        // from its own i64 counter; the push half has already delivered, so
        // the row is simply not seated and the fault is on the record.
        tracing::warn!(
            message = %event.message_id.to_text(),
            conversation = %event.conversation_id.to_text(),
            seq = event.seq,
            "a crossed message carries a sequence the store cannot seat, its row is not held"
        );
        return;
    };
    let message_id = replica.message_id;
    let conversation_id = replica.conversation_id;
    match store.replicate_message(replica).await {
        Ok(seated) => {
            if !seated {
                tracing::debug!(
                    message = %message_id.to_text(),
                    conversation = %conversation_id.to_text(),
                    "a crossed message was already held; the arrival seated nothing"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                message = %message_id.to_text(),
                conversation = %conversation_id.to_text(),
                "cannot seat a crossed message row; the live push still stands"
            );
        }
    }
}

/// Reads a wire message event back into the store's own terms.
///
/// Every field is verbatim except the two the wire cannot say plainly: the
/// sending device is the protocol's nil id when the origin recorded none, and
/// the tombstone becomes the receiving clock, per [`seat`]'s contract.
fn replica_of(event: &MessageEvent, now: Timestamp) -> Option<ReplicaMessage> {
    // The wire's seq is u64 and the store's is i64; the origin assigned it
    // from its own i64 counter, so a value that does not fit is a fault, not
    // a number to clamp — clamping would renumber what the sender sequenced.
    let seq = i64::try_from(event.seq).ok()?;
    Some(ReplicaMessage {
        message_id: event.message_id,
        conversation_id: event.conversation_id,
        seq,
        sender_id: event.sender_id,
        sender_device: (!event.sender_device.is_nil()).then_some(event.sender_device),
        kind: event.kind,
        envelope: event.envelope.clone(),
        reply_to: event.reply_to,
        created_at: event.created_at,
        edited_at: event.edited_at,
        deleted_at: (event.deleted == Some(true)).then_some(now),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::Id;
    use migo_protocol::MessageKind;

    fn event(seq: u64, deleted: Option<bool>) -> MessageEvent {
        MessageEvent {
            message_id: Id::from(70u128),
            conversation_id: Id::from(50u128),
            seq,
            sender_id: Id::from(1u128),
            sender_device: Id::NIL,
            kind: MessageKind::Text,
            envelope: vec![9, 9, 9, 9],
            created_at: Timestamp::from_millis(3_000),
            reply_to: None,
            edited_at: None,
            deleted,
            sender_key_id: None,
        }
    }

    #[test]
    fn a_plain_event_reads_back_verbatim_with_no_device_and_no_expiry_word() {
        let replica = replica_of(&event(7, None), Timestamp::from_millis(3_100)).unwrap();
        assert_eq!(replica.message_id, Id::from(70u128));
        assert_eq!(replica.conversation_id, Id::from(50u128));
        assert_eq!(replica.seq, 7);
        assert_eq!(
            replica.sender_device, None,
            "the protocol's nil device reads back as no device"
        );
        assert_eq!(replica.envelope, vec![9, 9, 9, 9]);
        assert_eq!(replica.edited_at, None);
        assert_eq!(
            replica.deleted_at, None,
            "a plain arrival is not a tombstone, whatever the clock says"
        );
    }

    #[test]
    fn a_tombstone_reads_back_as_the_receiving_clock() {
        let replica = replica_of(&event(7, Some(true)), Timestamp::from_millis(3_200)).unwrap();
        assert_eq!(
            replica.deleted_at,
            Some(Timestamp::from_millis(3_200)),
            "the wire carries no deletion time, so the row records when this node learned"
        );
    }

    #[test]
    fn a_deleted_false_is_not_a_tombstone() {
        let replica = replica_of(&event(7, Some(false)), Timestamp::from_millis(3_200)).unwrap();
        assert_eq!(
            replica.deleted_at, None,
            "the flag is tri-state on the wire and only Some(true) deletes"
        );
    }

    #[test]
    fn a_seq_the_store_cannot_number_is_refused_rather_than_clamped() {
        assert!(replica_of(&event(u64::MAX, None), Timestamp::from_millis(3_000)).is_none());
    }

    #[test]
    fn a_recorded_device_reads_back_verbatim() {
        let mut wire = event(7, None);
        wire.sender_device = Id::from(9u128);
        let replica = replica_of(&wire, Timestamp::from_millis(3_100)).unwrap();
        assert_eq!(replica.sender_device, Some(Id::from(9u128)));
    }

    #[test]
    fn an_encoded_event_round_trips_through_the_read_back() {
        // The ingest path decodes the frame before this module ever sees it;
        // this pins that the wire's own encoding is the shape the read-back
        // expects, so a schema drift on either side fails here first.
        let wire = event(7, Some(true));
        let frame =
            migo_protocol::to_frame(migo_protocol::Opcode::MessageEvent.to_wire(), 1, &wire)
                .unwrap();
        let decoded: MessageEvent =
            migo_protocol::from_frame(&frame).expect("the wire shape round-trips");
        let replica = replica_of(&decoded, Timestamp::from_millis(3_300)).unwrap();
        assert_eq!(replica.seq, 7);
        assert_eq!(replica.deleted_at, Some(Timestamp::from_millis(3_300)));
    }
}
