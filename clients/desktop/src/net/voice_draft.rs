//! Voice-note drafts: the on-disk half of a recording.
//!
//! Section 179's rule is that a recording is written *incrementally* into the app's private
//! storage while it runs — never held whole in memory — so an app that dies mid-recording
//! leaves its bytes on disk, and the next open of the conversation finds them as a draft.
//! The Android client keeps this store in its `filesDir`; this one keeps it beside the
//! vault, in the platform's own per-user config directory, which is the desktop's private
//! storage the same way.
//!
//! The store is deliberately dumb, the Android twin's own rule: it holds and describes
//! bytes, and never judges them. The recording itself is an Ogg Opus stream — the pages
//! the recorder's encoder writes as its frames fill — appended by the capture pump as the
//! chunks arrive, so the draft exists from the microphone's first moment and is a playable
//! Ogg from its first page. The descriptor beside it — a small fixed-layout file, rewritten
//! on the recording's own tick — carries the facts the composer needs before anyone can
//! read the audio back: how long it had run and the amplitudes sampled so far. Which
//! conversation a draft belongs to is the filenames' own fact, and a descriptor whose
//! recording file has vanished reads back as nothing rather than as a reference to nothing.
//!
//! One relic is kept deliberately: a previous release of this client recorded raw PCM at
//! 8 kHz into a `.pcm` file, and a draft it left behind is still a draft worth recovering.
//! The legacy file is read back as the samples it is and re-encoded by the caller; the
//! store itself only knows that the old name existed.

use std::fs;
use std::path::{Path, PathBuf};

use migo_core::Id;

/// One draft, as the composer meets it after a stop or an app death: everything the preview
/// and the send need, with the bytes staying in the store until one of them asks.
pub(crate) struct VoiceDraft {
    /// The playing time the descriptor last stated — the tick's own count, which stands
    /// still through a pause exactly as the recording does. The recorder's own statement of
    /// how long it ran: a death between ticks can leave it up to a second behind the bytes,
    /// and the send settles on the bytes' own count, but the preview the recovery shows is
    /// this one — the last thing the recording said about itself.
    pub duration_ms: u64,
    /// The sampled amplitude bars, 0–255, unfolded — the fold is a send-time judgement,
    /// and a draft recovered for preview shows the same live bars it recorded.
    pub amplitudes: Vec<u8>,
}

/// The draft store: one draft per conversation, in the app's private config directory.
pub(crate) struct VoiceDraftStore {
    dir: PathBuf,
}

impl VoiceDraftStore {
    /// The store beside the vault — `voice-note-drafts/` under the same directory the
    /// encrypted vault lives in, so the two never disagree about where this device keeps
    /// its private bytes.
    pub(crate) fn beside(vault: &Path) -> Self {
        let dir = vault
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("voice-note-drafts");
        Self { dir }
    }

    /// Where the conversation's recording is written — one draft per conversation, by
    /// design: the second recording a conversation starts is the first draft's overwrite.
    fn note_path(&self, conversation_id: Id) -> PathBuf {
        self.dir.join(format!("{}.ogg", conversation_id.to_text()))
    }

    /// Where the previous release wrote its raw PCM — the file a legacy draft's bytes live
    /// in, read back only to be re-encoded into the note the send expects.
    fn legacy_pcm_path(&self, conversation_id: Id) -> PathBuf {
        self.dir.join(format!("{}.pcm", conversation_id.to_text()))
    }

    /// Where the conversation's descriptor lives, rewritten on the recording's tick.
    fn descriptor_path(&self, conversation_id: Id) -> PathBuf {
        self.dir
            .join(format!("{}.draft", conversation_id.to_text()))
    }

    /// Creates the recording's file, truncating any draft before it. The capture pump owns
    /// the handle from here — the encoder the pump feeds writes its pages into it — and the
    /// store never touches the bytes again until they are read back whole.
    pub(crate) fn create_note(&self, conversation_id: Id) -> std::io::Result<fs::File> {
        fs::create_dir_all(&self.dir)?;
        fs::File::create(self.note_path(conversation_id))
    }

    /// Writes a note's bytes in place of whatever the conversation's draft held — the
    /// migration path's own hand: a legacy draft, re-encoded, becomes the note it now is.
    pub(crate) fn write_note(&self, conversation_id: Id, bytes: &[u8]) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        fs::write(self.note_path(conversation_id), bytes)
    }

    /// Persists the descriptor beside the recording's own file, best-effort: a descriptor
    /// write that fails must not take the recording with it, and the next tick's rewrite is
    /// the retry — the worst a missed write costs is a second of the timer's precision in a
    /// draft recovered after a death.
    pub(crate) fn save(&self, conversation_id: Id, duration_ms: u64, amplitudes: &[u8]) {
        let mut descriptor = Vec::with_capacity(8 + 4 + amplitudes.len());
        descriptor.extend_from_slice(&duration_ms.to_le_bytes());
        descriptor.extend_from_slice(
            &u32::try_from(amplitudes.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        descriptor.extend_from_slice(amplitudes);
        let _ = fs::write(self.descriptor_path(conversation_id), descriptor);
    }

    /// Reads the conversation's draft, or `None` when none exists. A descriptor whose
    /// recording file has vanished — the note and the legacy PCM both — or either half
    /// that cannot be read, is no draft at all: the store describes bytes it can point at,
    /// never bytes it can only name.
    pub(crate) fn load(&self, conversation_id: Id) -> Option<VoiceDraft> {
        let descriptor = fs::read(self.descriptor_path(conversation_id)).ok()?;
        if descriptor.len() < 12
            || !(self.note_path(conversation_id).is_file()
                || self.legacy_pcm_path(conversation_id).is_file())
        {
            return None;
        }
        let duration_ms = u64::from_le_bytes(descriptor[0..8].try_into().ok()?);
        let count = u32::from_le_bytes(descriptor[8..12].try_into().ok()?) as usize;
        if descriptor.len() < 12 + count {
            return None;
        }
        Some(VoiceDraft {
            duration_ms,
            amplitudes: descriptor[12..12 + count].to_vec(),
        })
    }

    /// Reads the draft's recorded note back, whole: the Ogg Opus stream the recorder's
    /// encoder wrote. An absent file is no note — the caller decides what that means.
    pub(crate) fn read_note(&self, conversation_id: Id) -> Option<Vec<u8>> {
        fs::read(self.note_path(conversation_id)).ok()
    }

    /// Reads the samples a legacy draft holds, whole: the raw PCM — mono, 16-bit, little
    /// endian, at the rate the previous release recorded — that a draft from before the
    /// Opus switch is. The odd-length file is refused rather than truncated: a PCM stream
    /// the old pump did not finish writing a sample into is a byte this client never
    /// recorded, and guessing which half to drop is a judgement the store does not make.
    pub(crate) fn read_legacy_samples(&self, conversation_id: Id) -> Option<Vec<i16>> {
        let bytes = fs::read(self.legacy_pcm_path(conversation_id)).ok()?;
        if bytes.len() % 2 != 0 {
            return None;
        }
        // `as_chunks` rather than `chunks_exact`: the file was already refused above for an
        // odd length, so the remainder half is empty by construction and the pairs are real
        // arrays the conversion can take whole.
        Some(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair))
                .collect(),
        )
    }

    /// Removes the legacy PCM half alone — the migration's own step, once the re-encoded
    /// note has taken its place beside the descriptor that always described both.
    pub(crate) fn clear_legacy(&self, conversation_id: Id) {
        let _ = fs::remove_file(self.legacy_pcm_path(conversation_id));
    }

    /// Removes the conversation's draft — the note, the descriptor describing it, and the
    /// legacy file that might predate the note. The one door out for a sent or abandoned
    /// note; everything else keeps the draft for the undo window or the recovery that
    /// follows a death.
    pub(crate) fn clear(&self, conversation_id: Id) {
        let _ = fs::remove_file(self.note_path(conversation_id));
        let _ = fs::remove_file(self.legacy_pcm_path(conversation_id));
        let _ = fs::remove_file(self.descriptor_path(conversation_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// A fresh store under a throwaway directory, so the tests never touch a real config
    /// directory. Unique per call, cleaned up on the way out.
    fn scratch(name: &str) -> (VoiceDraftStore, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("migo-voice-draft-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the scratch directory is created");
        let vault = dir.join("vault.bin");
        fs::write(&vault, b"not a real vault, only a place to stand beside")
            .expect("the vault stand-in");
        (VoiceDraftStore::beside(&vault), dir)
    }

    /// The round trip the recovery path depends on: a descriptor and its recording are
    /// written by the recording's own tick and pump, and the next open of the conversation
    /// reads back exactly what they stated.
    #[test]
    fn a_draft_round_trips_through_the_store() {
        let (store, dir) = scratch("round-trip");
        let conversation = Id::from_bytes([7; 16]);
        let mut note = store
            .create_note(conversation)
            .expect("the recording's file is created");
        // The bytes stand in for the encoder's pages — the store never judges them.
        note.write_all(b"OggS-not-really-but-the-store-does-not-look")
            .expect("pages append");
        drop(note);
        store.save(conversation, 2_500, &[3, 40, 250]);

        let draft = store.load(conversation).expect("the draft reads back");
        assert_eq!(draft.duration_ms, 2_500);
        assert_eq!(draft.amplitudes, vec![3, 40, 250]);
        assert_eq!(
            store.read_note(conversation).expect("the note reads back"),
            b"OggS-not-really-but-the-store-does-not-look".as_slice()
        );
        // The fold is the send-time judgement, not the store's: the draft keeps its raw
        // bars and the fold is taken when the message is built — here, one sample per
        // bucket, so the raw bars keep their own places.
        let mut expected = vec![0u8; super::super::media::WAVEFORM_BARS];
        expected[0] = 3;
        expected[1] = 40;
        expected[2] = 250;
        assert_eq!(
            super::super::media::downsample_waveform(&draft.amplitudes),
            expected
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// Half a draft is no draft: a descriptor whose recording file has vanished reads back
    /// as nothing, and a recording file with no descriptor is nothing to describe it.
    #[test]
    fn a_draft_missing_either_half_reads_as_none() {
        let (store, dir) = scratch("halves");
        let conversation = Id::from_bytes([9; 16]);
        assert!(store.load(conversation).is_none());

        store.save(conversation, 1_000, &[5]);
        // The descriptor exists but the recording never wrote a page: no draft.
        assert!(store.load(conversation).is_none());
        // ...and the other way around, the recording without its descriptor.
        drop(
            store
                .create_note(conversation)
                .expect("the file is created"),
        );
        let _ = fs::remove_file(
            dir.join("voice-note-drafts")
                .join(format!("{}.draft", conversation.to_text())),
        );
        assert!(store.load(conversation).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    /// A draft the previous release left as raw PCM still reads as a draft — the descriptor
    /// describes it, the legacy samples read back as the PCM they are — and its note half
    /// is empty until the caller re-encodes it into place.
    #[test]
    fn a_legacy_pcm_draft_is_still_a_draft() {
        let (store, dir) = scratch("legacy");
        let conversation = Id::from_bytes([11; 16]);
        let legacy = dir.join("voice-note-drafts");
        fs::create_dir_all(&legacy).expect("the drafts directory");
        fs::write(
            legacy.join(format!("{}.pcm", conversation.to_text())),
            [1u8, 0, 0xFF, 0],
        )
        .expect("the legacy samples are written");
        store.save(conversation, 2_000, &[9]);

        let draft = store
            .load(conversation)
            .expect("the legacy draft reads back");
        assert_eq!(draft.duration_ms, 2_000);
        assert_eq!(
            store
                .read_legacy_samples(conversation)
                .expect("the legacy samples read back"),
            vec![1, 255]
        );
        assert!(
            store.read_note(conversation).is_none(),
            "the note half does not exist until the migration writes it"
        );

        // The migration's own halves: the re-encoded note lands in place, and the legacy
        // file leaves alone.
        store
            .write_note(conversation, b"OggS")
            .expect("the note is written");
        store.clear_legacy(conversation);
        assert!(store.read_legacy_samples(conversation).is_none());
        assert_eq!(
            store.read_note(conversation).expect("the note reads back"),
            b"OggS".as_slice()
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// `clear` removes every half, so a cleared conversation recovers nothing the next time
    /// it opens — the sent note and the abandoned one leave no draft behind, not even the
    /// legacy file an older release might have left.
    #[test]
    fn clearing_removes_every_half() {
        let (store, dir) = scratch("clear");
        let conversation = Id::from_bytes([3; 16]);
        drop(
            store
                .create_note(conversation)
                .expect("the file is created"),
        );
        store.save(conversation, 500, &[9]);
        assert!(store.load(conversation).is_some());
        store.clear(conversation);
        assert!(store.load(conversation).is_none());
        assert!(store.read_note(conversation).is_none());
        assert!(store.read_legacy_samples(conversation).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    /// A second recording for the same conversation overwrites the first draft's file, the
    /// one-draft-per-conversation rule's own mechanics: the new recording starts at zero
    /// rather than appending onto the abandoned note's tail.
    #[test]
    fn a_new_recording_starts_its_file_over() {
        let (store, dir) = scratch("overwrite");
        let conversation = Id::from_bytes([5; 16]);
        drop(
            store
                .create_note(conversation)
                .expect("the first file is created"),
        );
        let mut second = store
            .create_note(conversation)
            .expect("the second file is created");
        second.write_all(b"OggS").expect("pages append");
        drop(second);
        assert_eq!(
            store.read_note(conversation).expect("only the new bytes"),
            b"OggS".as_slice()
        );
        let _ = fs::remove_dir_all(dir);
    }
}
