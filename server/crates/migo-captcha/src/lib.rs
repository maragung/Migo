//! Image captcha challenges for the public bootstrap surface.
//!
//! A challenge is a picture: five to six letters and digits, upper-case only, drawn from
//! an alphabet that leaves out the characters a screen renders ambiguously — `I`, `O`, `S`
//! and the digits `0`, `1`, `5` — over a noisy ground, with per-character rotation, scale,
//! and baseline jitter, and a single interference curve threaded through every character
//! (the renderer in [`render`] owns the whole pipeline and its guarantees). The user reads
//! the picture and types what they see; the server holds only an HMAC tag of the answer,
//! never the answer, and the challenge dies the moment it is answered.
//!
//! # Why an image, after all
//!
//! The first version of this crate was a plain numeric code carried on the wire, on the
//! theory that a behavioural signal was enough for a friend-or-bot gate. That posture
//! aged badly for one reason: a numeric challenge in the response body is solvable by
//! whatever script is reading the body, so the gate only slowed a scripted caller by one
//! request. An image the server renders and never describes in text moves the solve from
//! reading a field to reading a picture, which is the entire point of a captcha. The
//! cost is real — a renderer, three embedded fonts, a PNG per challenge — and it is paid
//! knowingly: fonts are embedded rather than read from the host so the difficulty of a
//! challenge is the same on every machine that builds or runs the server.
//!
//! # What the server keeps, and for how long
//!
//! The store holds `challenge_id -> tag`, where the tag is the HMAC-SHA-256 of the
//! normalised answer under a key derived from the same `MacKey` root every other
//! short-lived Migo token uses, under this crate's own label. A database dump yields no
//! answers; a captured challenge cannot be replayed as another kind of token. Challenges
//! expire quickly (default two minutes) and are one-shot by construction:
//! [`CaptchaStore::consume`] removes the row in the same operation that reads it, so two
//! racing answers to the same challenge cannot both succeed.
//!
//! # Answers and their normalisation
//!
//! What the user types is compared case-insensitively and whitespace-insensitively — the
//! answer is upper-cased and stripped of all whitespace before it is hashed on either
//! side. The comparison is constant-time over the tag. A wrong answer, an expired
//! challenge, an unknown id, and a second attempt at a solved challenge all answer the
//! same `false`, because which of those it was is nobody's business but the server's.
//!
//! # The accessible alternative
//!
//! [`CaptchaMode::ImageAlt`] issues a fresh challenge — a new random code, never the same
//! answer read aloud — rendered with larger glyphs, less rotation, a thinner and fainter
//! curve, and a fixed high-contrast ground. It exists because a distorted picture is a
//! reading test some humans fail through no fault of their own; it is still a picture,
//! because the alternative path must remain a captcha, not a bypass.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use migo_core::config::CaptchaConfig;
use migo_core::{Clock, Id, Random, Result, Timestamp};
use migo_crypto::MacKey;

mod render;

/// The HMAC label that separates captcha answer tags from every other server token
/// (`LABEL_VERIFICATION`, `LABEL_SESSION_TOKEN`, ...). Two labels that ever shared a key
/// would let a captured token of one kind be replayed as the other if the payload shapes
/// ever converged; this one is versioned because the tag's meaning — "the answer to a
/// challenge", not the v1 idea of a challenge id — changed when the crate became an
/// image captcha.
pub const LABEL: &[u8] = b"migo-captcha-v2";

/// Long-form alias for [`LABEL`], used by callers that prefer the explicit name in
/// composition code where a one-letter constant could be read as something other than a
/// label.
pub const LABEL_CAPTCHA_CHALLENGE: &[u8] = LABEL;

/// The challenge alphabet: upper-case letters and digits, minus the ones a screen renders
/// ambiguously. `I`/`1`, `O`/`0`, and `S`/`5` are out; everything else stays, because a
/// challenge that removed every confusable pair would have no alphabet left worth typing.
pub const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRTUVWXYZ2346789";

/// The default lifetime of a challenge, in seconds. The source of truth is
/// [`CaptchaConfig::ttl_seconds`]; this constant exists so the crate's documentation and
/// the configuration default cannot drift apart unnoticed — a test pins their equality.
pub const DEFAULT_TTL_SECONDS: u32 = 120;

/// Which rendering a challenge asks for.
///
/// [`Self::Image`] is the standard challenge. [`Self::ImageAlt`] is the accessible
/// alternative: a freshly-issued challenge with a different random code and gentler
/// rendering parameters, for users who cannot read the standard one. Serialises as
/// `"image"` and `"image_alt"` on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptchaMode {
    /// The standard distorted challenge.
    Image,
    /// The high-contrast, larger-glyph alternative. Still an image, still a fresh code.
    ImageAlt,
}

/// A challenge as the store keeps it: an id, the tag of the answer, and the expiry.
///
/// The answer itself exists only inside [`CaptchaService::issue`] for the length of one
/// render, which is the only moment the picture and the answer are in the same place.
#[derive(Clone, Debug)]
pub struct Challenge {
    /// Random per-challenge id; surfaces to the client as a JSON field.
    pub challenge_id: Id,
    /// The HMAC-SHA-256 of the normalised answer. Not the answer, and not invertible
    /// into one without the process's key.
    pub tag: [u8; 32],
    /// When the challenge stops being accepted. The half-open test (`<`) is what every
    /// other TTL in the codebase uses.
    pub expires_at: Timestamp,
}

impl Challenge {
    /// Whether `now` is still within the window the user is allowed to answer.
    #[must_use]
    pub fn valid_at(&self, now: Timestamp) -> bool {
        now < self.expires_at
    }
}

/// The captcha proof a client sends back when answering a challenge.
///
/// The two fields are the only ones the wire carries: the id the server gave the user and
/// the characters they read off the image. Whether the proof is right is a server-side
/// comparison over the tag, in [`CaptchaService::verify`].
#[derive(Clone, Debug)]
pub struct CaptchaProof {
    /// The id of the challenge this proof answers.
    pub challenge_id: Id,
    /// What the user read off the image. Normalised server-side: upper-cased, stripped
    /// of all whitespace, so how the user's keyboard capitalises is never the reason a
    /// correct answer is refused.
    pub answer: String,
}

/// The captcha challenge as it appears on the wire.
///
/// The picture is the whole question: a base64-encoded PNG the client renders directly,
/// with no textual description of it anywhere in the response. A screen reader announces
/// the image's alt text and the user who cannot solve it asks for
/// [`CaptchaMode::ImageAlt`] — the accessible path is a different challenge, never a
/// disclosed answer.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CaptchaChallengeView {
    /// The id the client echoes back as the proof's `challenge_id`.
    pub challenge_id: Id,
    /// The rendered challenge, standard base64 with padding, ready for a
    /// `data:image/png;base64,` URL.
    pub image_png_base64: String,
    /// The mode this challenge was rendered in, echoed so the client knows which of its
    /// two buttons refreshes in kind.
    pub mode: CaptchaMode,
    /// The seconds the challenge stays valid after issuance.
    pub ttl_seconds: u32,
}

/// Storage for live challenges.
///
/// The shape is deliberately two methods, not three: [`Self::consume`] removes and
/// returns a challenge in one operation, because a one-shot guarantee that is only as
/// strong as "get, then delete" is not a guarantee at all under concurrency — two
/// answers racing down the wire would both read the row before either removed it.
#[async_trait]
pub trait CaptchaStore: Send + Sync {
    /// Persists `challenge` until its `expires_at`. A second call with the same
    /// `challenge_id` replaces the first.
    async fn put(&self, challenge: &Challenge) -> Result<()>;

    /// Atomically removes the challenge and returns it, if it is still live.
    ///
    /// `None` when the challenge is absent, already consumed, or expired at `now`. The
    /// caller never needs a separate delete, and no two callers can both receive the
    /// same challenge.
    async fn consume(&self, challenge_id: Id, now: Timestamp) -> Result<Option<Challenge>>;
}

/// The service the API hands a challenge to. One per process; built from the same
/// `MacKey` root every other short-lived server token on Migo uses, with the captcha
/// label applied so a captured tag can never be replayed as a verification or session
/// token.
pub struct CaptchaService {
    /// The HMAC key the answer tags are computed under.
    key: MacKey,
    /// The deployment's rendering and policy knobs.
    config: CaptchaConfig,
    clock: Arc<dyn Clock + Send + Sync>,
}

/// The uppercase, whitespace-free form every answer — issued or submitted — is hashed in.
///
/// Case-insensitive and space-insensitive comparison falls out of normalising both sides
/// the same way, which is the only way a user's keyboard habits can fail a correct read.
fn normalise(answer: &str) -> String {
    answer
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_uppercase()
}

impl CaptchaService {
    /// Builds a service from a 32-byte secret root, a clock, and the deployment's
    /// captcha configuration.
    #[must_use]
    pub fn new(
        secret_root: &[u8],
        clock: Arc<dyn Clock + Send + Sync>,
        config: CaptchaConfig,
    ) -> Self {
        Self {
            key: MacKey::derive(secret_root, LABEL),
            config,
            clock,
        }
    }

    /// The tag of an answer under this service's key.
    fn tag_of(&self, answer: &str) -> [u8; 32] {
        self.key.tag(normalise(answer).as_bytes())
    }

    /// Mints, renders, and stores nothing: that is the caller's half. Returns the view
    /// to put on the wire and the challenge to put in the store, whose tag is the only
    /// form of the answer that leaves this call.
    fn issue_inner<R: Random>(
        &self,
        mode: CaptchaMode,
        random: &mut R,
    ) -> Result<(CaptchaChallengeView, Challenge, String)> {
        let now = self.clock.now();
        let length = self.config.length_min.saturating_add(
            (random.next_u64()
                % u64::from(
                    self.config
                        .length_max
                        .saturating_sub(self.config.length_min)
                        + 1,
                )) as u8,
        );
        let mut answer = String::with_capacity(length as usize);
        for _ in 0..length {
            let index = (random.next_u64() % ALPHABET.len() as u64) as usize;
            answer.push(ALPHABET[index] as char);
        }

        let params = render::RenderParams {
            width: self.config.image_width,
            height: self.config.image_height,
            noise: self.config.noise_strength,
            accessible: mode == CaptchaMode::ImageAlt,
        };
        let png = render::render(&answer, &params, random)?;

        let expires_at = now.saturating_add_millis(i64::from(self.config.ttl_seconds) * 1_000);
        let challenge_id = Id::generate_at(now, random);
        let challenge = Challenge {
            challenge_id,
            tag: self.tag_of(&answer),
            expires_at,
        };
        let view = CaptchaChallengeView {
            challenge_id,
            image_png_base64: base64::engine::general_purpose::STANDARD.encode(&png),
            mode,
            ttl_seconds: self.config.ttl_seconds,
        };
        Ok((view, challenge, answer))
    }

    /// Mints a fresh challenge with the system entropy source. Production paths use
    /// this; tests that need determinism use [`Self::issue_with`].
    ///
    /// # Errors
    ///
    /// Only a rendering failure — an unusable embedded font or a PNG encoder fault —
    /// neither of which a healthy process can produce twice.
    pub fn issue(&self, mode: CaptchaMode) -> Result<(CaptchaChallengeView, Challenge)> {
        let (view, challenge, _answer) = self.issue_inner(mode, &mut migo_core::OsRandom)?;
        Ok((view, challenge))
    }

    /// Mints a fresh challenge from an injected randomness source, for reproducibility.
    ///
    /// # Errors
    ///
    /// As [`Self::issue`].
    pub fn issue_with<R: Random>(
        &self,
        mode: CaptchaMode,
        random: &mut R,
    ) -> Result<(CaptchaChallengeView, Challenge)> {
        let (view, challenge, _answer) = self.issue_inner(mode, random)?;
        Ok((view, challenge))
    }

    /// Mints a fresh challenge and also returns the plaintext answer, so an integration
    /// suite can complete the challenge it just issued.
    ///
    /// Behind the `test-internal` feature — nothing on a production path enables it, and
    /// the feature is only ever pulled in by dev-dependencies — plus this crate's own
    /// tests, which compile under `cfg(test)`. This is the one door out for the answer,
    /// it is labelled, and it stays shut in release builds.
    #[cfg(any(feature = "test-internal", test))]
    pub fn issue_for_test<R: Random>(
        &self,
        mode: CaptchaMode,
        random: &mut R,
    ) -> Result<(CaptchaChallengeView, Challenge, String)> {
        self.issue_inner(mode, random)
    }

    /// Validates `(challenge_id, submitted)` against the stored challenge.
    ///
    /// Returns `true` only for a live challenge whose normalised answer hashes to the
    /// stored tag, and consumes the challenge on every path — right, wrong, or expired —
    /// so a challenge can never be answered twice, successfully or otherwise.
    pub async fn verify<S: CaptchaStore + ?Sized>(
        &self,
        store: &S,
        challenge_id: Id,
        submitted: &str,
    ) -> Result<bool> {
        let now = self.clock.now();
        // The consume is the one-shot guarantee: whatever happens next, the challenge is
        // gone, and a racing second answer finds nothing.
        let Some(challenge) = store.consume(challenge_id, now).await? else {
            return Ok(false);
        };
        if !challenge.valid_at(now) {
            return Ok(false);
        }
        let submitted = normalise(submitted);
        let length = u8::try_from(submitted.len()).unwrap_or(u8::MAX);
        let in_alphabet = submitted.bytes().all(|byte| ALPHABET.contains(&byte));
        if !in_alphabet || !(self.config.length_min..=self.config.length_max).contains(&length) {
            return Ok(false);
        }
        Ok(constant_time_eq(
            &self.key.tag(submitted.as_bytes()),
            &challenge.tag,
        ))
    }
}

/// Constant-time byte compare. We do not pull in `subtle` for one helper; the length
/// mismatch path returns early, which leaks only the length — a value the wire already
/// knows.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Default in-memory store, and the one a single-process migod uses. A multi-replica
/// deployment replaces it behind the same trait, pointed at a shared row store.
///
/// Expiry is enforced on consume by the caller-supplied `now`, so the store needs no
/// clock of its own: a stale row is dropped the moment somebody asks for it, which is
/// also the moment its id becomes unforgable, because the row is gone.
pub struct InMemoryStore {
    inner: parking_lot::Mutex<std::collections::HashMap<Id, Challenge>>,
}

impl InMemoryStore {
    /// Builds an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CaptchaStore for InMemoryStore {
    async fn put(&self, challenge: &Challenge) -> Result<()> {
        self.inner
            .lock()
            .insert(challenge.challenge_id, challenge.clone());
        Ok(())
    }

    async fn consume(&self, challenge_id: Id, now: Timestamp) -> Result<Option<Challenge>> {
        // remove() and the expiry test under one lock: the second consumer of an id
        // finds the row already gone, whichever task the runtime interleaves first.
        let removed = self.inner.lock().remove(&challenge_id);
        match removed {
            Some(challenge) if challenge.valid_at(now) => Ok(Some(challenge)),
            _ => Ok(None),
        }
    }
}

/// Validates that a normalised answer has the shape worth comparing: nothing outside the
/// challenge alphabet, and a length the configuration's validator allows. Used to fail
/// fast on malformed input; the authoritative check is the tag comparison.
#[must_use]
pub fn is_well_formed(normalised: &str) -> bool {
    (4..=8).contains(&normalised.len()) && normalised.bytes().all(|byte| ALPHABET.contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::{ManualClock, SeededRandom};

    fn service() -> (CaptchaService, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let svc = CaptchaService::new(b"a-secret", clock.clone(), CaptchaConfig::default());
        (svc, clock)
    }

    #[test]
    fn the_alphabet_leaves_out_what_a_screen_renders_ambiguously() {
        for ambiguous in b"IOS015" {
            assert!(
                !ALPHABET.contains(ambiguous),
                "{ambiguous} must not be in the alphabet"
            );
        }
        assert!(ALPHABET.contains(&b'A'));
        assert!(ALPHABET.contains(&b'9'));
    }

    #[test]
    fn the_documented_default_ttl_matches_the_configuration_default() {
        assert_eq!(DEFAULT_TTL_SECONDS, CaptchaConfig::default().ttl_seconds);
    }

    #[test]
    fn well_formed_accepts_only_alphabet_characters_in_range() {
        assert!(is_well_formed("AB3D7"));
        assert!(is_well_formed("AB3D7K"));
        assert!(!is_well_formed("AB3")); // too short for any configuration
        assert!(!is_well_formed("AB3D7IOS")); // contains the excluded characters
        assert!(!is_well_formed("AB3D7 ")); // whitespace, though callers normalise first
    }

    #[test]
    fn normalisation_is_uppercase_and_whitespace_free() {
        assert_eq!(normalise("ab3d7"), "AB3D7");
        assert_eq!(normalise(" a B 3 d 7 "), "AB3D7");
        assert_eq!(normalise("AB\t3D\n7"), "AB3D7");
    }

    #[test]
    fn an_issued_challenge_is_a_decodable_png_of_the_configured_size() {
        let (svc, _clock) = service();
        let (view, _challenge) = svc.issue(CaptchaMode::Image).expect("rendering succeeds");
        let png = base64::engine::general_purpose::STANDARD
            .decode(&view.image_png_base64)
            .expect("the view carries standard base64");
        assert_eq!(&png[..4], b"\x89PNG", "the bytes are a PNG");
        let decoded = image::load_from_memory(&png).expect("the PNG decodes");
        assert_eq!(decoded.width(), CaptchaConfig::default().image_width);
        assert_eq!(decoded.height(), CaptchaConfig::default().image_height);
        assert_eq!(view.ttl_seconds, CaptchaConfig::default().ttl_seconds);
        assert_eq!(view.mode, CaptchaMode::Image);
    }

    #[test]
    fn the_same_seed_renders_the_same_picture_and_the_same_answer() {
        let (svc, _clock) = service();
        let (first_view, first, first_answer) = svc
            .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(42))
            .expect("rendering succeeds");
        let (second_view, second, second_answer) = svc
            .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(42))
            .expect("rendering succeeds");
        assert_eq!(first_view.image_png_base64, second_view.image_png_base64);
        assert_eq!(first.tag, second.tag);
        assert_eq!(first_answer, second_answer);
    }

    #[test]
    fn no_two_live_challenges_share_a_picture_or_a_tag() {
        let (svc, _clock) = service();
        let mut pictures = std::collections::HashSet::new();
        let mut tags = std::collections::HashSet::new();
        for seed in 0..16u64 {
            let (view, challenge, _answer) = svc
                .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(seed))
                .expect("rendering succeeds");
            pictures.insert(view.image_png_base64);
            tags.insert(challenge.tag);
        }
        assert_eq!(pictures.len(), 16, "every challenge renders a unique image");
        assert_eq!(tags.len(), 16, "every challenge carries a unique tag");
    }

    #[test]
    fn the_alternative_mode_renders_a_different_usable_challenge() {
        let (svc, _clock) = service();
        let (standard, _first, _answer) = svc
            .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(7))
            .expect("rendering succeeds");
        let (alternative, _second, _answer) = svc
            .issue_for_test(CaptchaMode::ImageAlt, &mut SeededRandom::new(7))
            .expect("rendering succeeds");
        assert_eq!(alternative.mode, CaptchaMode::ImageAlt);
        assert_ne!(
            standard.image_png_base64, alternative.image_png_base64,
            "the accessible mode is a different challenge, not the same picture gentler"
        );
        let png = base64::engine::general_purpose::STANDARD
            .decode(&alternative.image_png_base64)
            .expect("still standard base64");
        assert_eq!(&png[..4], b"\x89PNG");
    }

    #[tokio::test]
    async fn a_correct_answer_verifies_case_and_whitespace_insensitively() {
        let (svc, _clock) = service();
        let store = InMemoryStore::new();
        let (_view, challenge, answer) = svc
            .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(1))
            .expect("rendering succeeds");
        store.put(&challenge).await.expect("stored");
        assert!(svc
            .verify(&store, challenge.challenge_id, &answer)
            .await
            .expect("verify answers"));
    }

    #[tokio::test]
    async fn a_lower_case_answer_with_spaces_verifies() {
        let (svc, _clock) = service();
        let store = InMemoryStore::new();
        let (_view, challenge, answer) = svc
            .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(2))
            .expect("rendering succeeds");
        store.put(&challenge).await.expect("stored");
        let spaced: String = answer
            .to_lowercase()
            .chars()
            .flat_map(|character| [' ', character])
            .collect();
        let typed = format!(" {spaced} ");
        assert!(
            svc.verify(&store, challenge.challenge_id, &typed)
                .await
                .expect("verify answers"),
            "the user's keyboard habits must not fail a correct read"
        );
    }

    #[tokio::test]
    async fn a_wrong_answer_is_refused_and_consumes_the_challenge() {
        let (svc, _clock) = service();
        let store = InMemoryStore::new();
        let (_view, challenge, answer) = svc
            .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(3))
            .expect("rendering succeeds");
        store.put(&challenge).await.expect("stored");
        let mut wrong = String::new();
        for byte in answer.bytes() {
            wrong.push(if byte == b'A' { 'B' } else { 'A' });
        }
        assert!(!svc
            .verify(&store, challenge.challenge_id, &wrong)
            .await
            .expect("verify answers"));
        // Wrong or right, the challenge is gone: the retry must also fail.
        assert!(!svc
            .verify(&store, challenge.challenge_id, &answer)
            .await
            .expect("verify answers"));
    }

    #[tokio::test]
    async fn an_expired_challenge_is_refused() {
        let (svc, clock) = service();
        let store = InMemoryStore::new();
        let (_view, challenge, answer) = svc
            .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(4))
            .expect("rendering succeeds");
        store.put(&challenge).await.expect("stored");
        clock.advance_millis(i64::from(CaptchaConfig::default().ttl_seconds) * 1_000 + 1);
        assert!(!svc
            .verify(&store, challenge.challenge_id, &answer)
            .await
            .expect("verify answers"));
    }

    #[tokio::test]
    async fn an_unknown_challenge_id_is_refused_quietly() {
        let (svc, _clock) = service();
        let store = InMemoryStore::new();
        assert!(!svc
            .verify(&store, Id::from(0xDEAD_u128), "AB3D7")
            .await
            .expect("verify answers"));
    }

    #[tokio::test]
    async fn two_racing_answers_cannot_both_succeed() {
        let (svc, _clock) = service();
        let store = std::sync::Arc::new(InMemoryStore::new());
        let (_view, challenge, answer) = svc
            .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(5))
            .expect("rendering succeeds");
        store.put(&challenge).await.expect("stored");

        let mut handles = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            let answer = answer.clone();
            let id = challenge.challenge_id;
            handles.push(tokio::spawn(async move {
                // Each racing task verifies through its own service clone so the test
                // exercises the store's atomicity, not a shared service's.
                let clock: Arc<dyn Clock + Send + Sync> =
                    Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
                let svc = CaptchaService::new(b"a-secret", clock, CaptchaConfig::default());
                svc.verify(store.as_ref(), id, &answer)
                    .await
                    .expect("verify answers")
            }));
        }
        let mut accepted = 0;
        for handle in handles {
            if handle.await.expect("task completes") {
                accepted += 1;
            }
        }
        assert_eq!(
            accepted, 1,
            "a one-shot challenge answers exactly one racing request"
        );
    }

    #[test]
    fn the_view_never_carries_the_answer_in_any_form() {
        let (svc, _clock) = service();
        let (view, _challenge, answer) = svc
            .issue_for_test(CaptchaMode::Image, &mut SeededRandom::new(6))
            .expect("rendering succeeds");
        let json = serde_json::to_string(&view).expect("the view serialises");
        assert!(
            !json.contains(&answer),
            "the answer must not appear in the wire JSON"
        );
        assert!(
            !json.contains(&answer.to_lowercase()),
            "not even in lower case"
        );
    }

    #[test]
    fn constant_time_eq_handles_length_mismatch() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(constant_time_eq(b"abc", b"abc"));
    }
}
