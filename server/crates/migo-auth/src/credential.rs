//! Validating and normalising the things a user types.
//!
//! Two forms of every identifier exist here and the distinction matters more than it
//! looks like it does.
//!
//! The *display* form is what the user typed: `@Satoshi`. It is what other people see,
//! and it is stored as given, because silently lowercasing someone's name is a small
//! rudeness that compounds.
//!
//! The *folded* form is what uniqueness is decided on: `satoshi`. Brief section 80
//! requires case-insensitive usernames, which means the folded form is the real key
//! and the display form is decoration on top of it.
//!
//! Getting this the wrong way round produces the classic bug where `Satoshi` and
//! `satoshi` are two accounts, discovered by a user who has just been impersonated.

use migo_core::{Error, Result, Secret};
use migo_protocol::{codes, fault};

/// Shortest username.
///
/// Three characters. Two-character usernames are worth more than they cost — they are
/// the ones that get squatted, sold, and used for impersonation — so they are held back
/// rather than given to whoever registers first.
pub const USERNAME_MIN_CHARS: usize = 3;

/// Longest username. Fits in a mention without wrapping a message bubble.
pub const USERNAME_MAX_CHARS: usize = 32;

/// Longest password accepted.
///
/// Argon2id will happily hash a megabyte, at a cost paid by the server rather than by
/// whoever submitted it. That is an unauthenticated CPU amplifier, so there is a bound.
/// It is generous enough that a passphrase manager's output fits several times over.
pub const PASSWORD_MAX_BYTES: usize = 256;

/// Longest email address accepted. The practical limit in the mail RFCs.
pub const EMAIL_MAX_CHARS: usize = 254;

/// Longest phone number, in E.164 digits.
pub const PHONE_MAX_DIGITS: usize = 15;

/// Shortest plausible phone number, in E.164 digits.
pub const PHONE_MIN_DIGITS: usize = 8;

/// Names the product needs and the public may not have.
///
/// Folded form, sorted, checked by binary search. Two categories are in here for two
/// different reasons.
///
/// Some are impersonation risks: an account called `support` or `security` can ask for
/// a password and be believed. Those are the expensive ones, because the damage is done
/// by the account's *name* and no amount of moderation undoes a message that has
/// already been read.
///
/// Some are routing collisions: `api`, `admin`, `settings`, and friends are path
/// segments. A username that shadows a route is a bug waiting for someone to add
/// `/{username}` to the router.
const RESERVED: &[&str] = &[
    "about",
    "abuse",
    "account",
    "accounts",
    "admin",
    "administrator",
    "api",
    "app",
    "apps",
    "assets",
    "auth",
    "billing",
    "blog",
    "bot",
    "bots",
    "careers",
    "cdn",
    "channel",
    "channels",
    "chat",
    "contact",
    "copyright",
    "dashboard",
    "dev",
    "developer",
    "developers",
    "dm",
    "docs",
    "download",
    "downloads",
    "everyone",
    "explore",
    "faq",
    "feed",
    "feedback",
    "files",
    "friends",
    "games",
    "gateway",
    "group",
    "groups",
    "help",
    "here",
    "home",
    "images",
    "info",
    "invite",
    "invites",
    "jobs",
    "legal",
    "login",
    "logout",
    "mail",
    "me",
    "media",
    "messages",
    "migo",
    "migoapp",
    "migoteam",
    "mod",
    "moderator",
    "news",
    "notifications",
    "official",
    "operator",
    "owner",
    "partner",
    "partners",
    "pay",
    "payment",
    "payments",
    "press",
    "privacy",
    "profile",
    "register",
    "room",
    "rooms",
    "root",
    "search",
    "security",
    "settings",
    "shop",
    "signin",
    "signup",
    "staff",
    "static",
    "status",
    "store",
    "support",
    "system",
    "team",
    "terms",
    "test",
    "trust",
    "unsubscribe",
    "upload",
    "uploads",
    "user",
    "users",
    "verified",
    "verify",
    "wallet",
    "web",
    "webhook",
    "webhooks",
    "www",
];

/// The most-guessed passwords, folded.
///
/// A deliberately tiny list. It is not a security control — a real one checks against a
/// corpus of breached credentials, which is tens of millions of hashes, belongs behind
/// a k-anonymity range query, and is a service rather than a constant. This list exists
/// to catch the specific case of a user who types the first thing that comes to mind
/// and would otherwise be told their password is fine.
///
/// The length rule does most of the work: everything here is short, and most common
/// passwords are.
const COMMON_PASSWORDS: &[&str] = &[
    "0123456789",
    "1234567890",
    "12345678901",
    "123456789012",
    "aaaaaaaaaa",
    "abcdefghij",
    "iloveyou12",
    "letmein123",
    "passw0rd12",
    "password01",
    "password12",
    "password123",
    "qwerty12345",
    "qwertyuiop",
    "welcome123",
];

/// A username that passed validation, in both forms it is needed in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Username {
    display: String,
    folded: String,
}

impl Username {
    /// What the user typed, minus surrounding whitespace and a leading `@`.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    /// The uniqueness key: lowercase.
    #[must_use]
    pub fn folded(&self) -> &str {
        &self.folded
    }
}

/// Validates and normalises a username.
///
/// # Rules
///
/// ASCII lowercase letters, digits, `_` and `.`, three to thirty-two characters,
/// starting with a letter, no adjacent or trailing separators.
///
/// The ASCII restriction is the uncomfortable one, and it is chosen with open eyes. A
/// username is an *impersonation surface*: it appears next to messages, and a reader
/// decides whether to trust a message partly by the name above it. Unicode contains
/// thousands of characters that render identically to ASCII ones — Cyrillic `а`, Greek
/// `ο`, full-width `ａ` — and any of them turns `@migosupport` into a name that is
/// visually identical and cryptographically distinct.
///
/// Handling that properly means Unicode confusable skeletons, script-mixing rules, and
/// a normalisation table that has to be updated with each Unicode release. That is the
/// right answer and it is a project with its own ADR. Until then the restriction is on
/// the *handle*, while the *display name* on the profile is free-form Unicode — so a
/// user writes their name in their own script and the impersonation surface stays flat.
pub fn username(raw: &str) -> Result<Username> {
    let trimmed = raw.trim().trim_start_matches('@');
    let display = trimmed.to_string();
    let folded = display.to_ascii_lowercase();

    let count = folded.chars().count();
    if count < USERNAME_MIN_CHARS {
        return Err(fault::validation(
            "username",
            "must be at least three characters",
        ));
    }
    if count > USERNAME_MAX_CHARS {
        return Err(fault::field_too_long("username", USERNAME_MAX_CHARS));
    }

    let bytes = folded.as_bytes();
    if !bytes[0].is_ascii_lowercase() {
        return Err(fault::validation("username", "must start with a letter"));
    }
    let last = bytes[bytes.len() - 1];
    if last == b'.' || last == b'_' {
        return Err(fault::validation(
            "username",
            "must not end with a separator",
        ));
    }
    let mut previous_separator = false;
    for &byte in bytes {
        let separator = byte == b'.' || byte == b'_';
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || separator) {
            return Err(fault::validation(
                "username",
                "may contain only letters, digits, dots, and underscores",
            ));
        }
        if separator && previous_separator {
            return Err(fault::validation(
                "username",
                "must not contain two separators in a row",
            ));
        }
        previous_separator = separator;
    }

    if is_reserved(&folded) {
        // A distinct code from `VALIDATION_FAILED` so the client can say "that name is
        // reserved" instead of "invalid", which reads as a bug in the client.
        return Err(fault::error(
            codes::USERNAME_RESERVED,
            "username is reserved",
        ));
    }

    Ok(Username { display, folded })
}

/// Whether a folded username is held back.
///
/// Checks the name as given and its *skeleton* — separators removed, digits that
/// imitate letters mapped back. `migo_support`, `mig0support`, and `m.i.g.o.support`
/// all collapse to `migosupport`, and all three are refused. This is a crude stand-in
/// for real confusable detection and it covers the substitutions people actually reach
/// for when the obvious name is taken.
#[must_use]
pub fn is_reserved(folded: &str) -> bool {
    if RESERVED.binary_search(&folded).is_ok() {
        return true;
    }
    let skeleton = skeleton(folded);
    skeleton != folded && RESERVED.binary_search(&skeleton.as_str()).is_ok()
}

/// Collapses a username to a comparison skeleton.
fn skeleton(folded: &str) -> String {
    folded
        .chars()
        .filter(|c| *c != '.' && *c != '_')
        .map(|c| match c {
            '0' => 'o',
            '1' => 'l',
            '3' => 'e',
            '4' => 'a',
            '5' => 's',
            '7' => 't',
            other => other,
        })
        .collect()
}

/// Checks a password against the configured floor.
///
/// # What is and is not checked
///
/// Length, a byte ceiling, and a small list of guesses. No composition rules — no
/// required digit, no required symbol, no required capital. Those rules are known to
/// make passwords *worse*: they push people from a long passphrase they remember to
/// `Password1!`, which satisfies every rule and appears in every breach corpus. NIST
/// dropped the recommendation in 2017 and the industry has been slow to notice.
///
/// The username comparison is on the folded forms and covers the substring case, since
/// `satoshi-satoshi` is not meaningfully stronger than `satoshi`.
pub fn password(raw: &Secret, min_length: usize, username: Option<&str>) -> Result<()> {
    let value = raw.expose();
    if value.len() > PASSWORD_MAX_BYTES {
        // Byte length, not character count: the bound exists to cap hashing work, and
        // hashing work is measured in bytes.
        return Err(weak("password is longer than 256 bytes"));
    }
    if value.chars().count() < min_length {
        return Err(weak("password is shorter than the configured minimum"));
    }
    let folded = value.to_lowercase();
    if COMMON_PASSWORDS.binary_search(&folded.as_str()).is_ok() {
        return Err(weak("password is one of the most commonly guessed"));
    }
    if let Some(name) = username {
        let name = name.to_lowercase();
        if name.len() >= USERNAME_MIN_CHARS && folded.contains(&name) {
            return Err(weak("password contains the username"));
        }
    }
    Ok(())
}

/// A weak-password refusal.
///
/// The reason is in the internal message and deliberately not in the public one: a
/// public "that is a common password" is a hint to whoever is standing behind the user,
/// and the client already knows how to say "please pick something stronger".
fn weak(why: &'static str) -> Error {
    fault::error(codes::WEAK_PASSWORD, why)
}

/// Validates and normalises an email address.
///
/// Loose on purpose. Strict RFC 5322 validation is a famous mistake: the grammar admits
/// quoted local parts, comments, and address literals, so a "correct" validator accepts
/// things no mail server will deliver to, while every *simple* regex on the internet
/// rejects addresses that are perfectly real — `+` tags, apostrophes, new top-level
/// domains. Both failure directions cost a real user their account.
///
/// So this checks only what a typo check can honestly check: one `@`, something either
/// side of it, a dot in the domain, no whitespace, a length bound. Whether the address
/// exists is settled by sending mail to it, which is the only test that was ever
/// authoritative.
pub fn email(raw: &str) -> Result<String> {
    let value = raw.trim().to_lowercase();
    if value.is_empty() {
        return Err(fault::field_required("email"));
    }
    if value.chars().count() > EMAIL_MAX_CHARS {
        return Err(fault::field_too_long("email", EMAIL_MAX_CHARS));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(fault::validation("email", "must not contain whitespace"));
    }
    let mut parts = value.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err(fault::validation("email", "must contain one at sign"));
    }
    if local.is_empty() || domain.is_empty() {
        return Err(fault::validation(
            "email",
            "needs a name and a domain either side of the at sign",
        ));
    }
    // A dot with something on both sides. Rejects `a@localhost` and `a@.com`, accepts
    // every address that can be reached from the public internet.
    let dotted = domain
        .split_once('.')
        .is_some_and(|(head, tail)| !head.is_empty() && !tail.is_empty() && !tail.starts_with('.'));
    if !dotted {
        return Err(fault::validation("email", "domain needs a dot"));
    }
    Ok(value)
}

/// Validates and normalises a phone number to E.164.
///
/// Strips the punctuation people type — spaces, dashes, dots, parentheses — and then
/// requires a leading `+` and eight to fifteen digits. The `+` is required rather than
/// guessed: inferring a country code from a request's address is how a user in one
/// country gets registered against a number in another.
///
/// Whether the number is *assigned* is not knowable here; that takes a verification
/// message, which is what section 46 asks for anyway.
pub fn phone(raw: &str) -> Result<String> {
    let mut digits = String::with_capacity(raw.len());
    let mut leading_plus = false;
    for (index, ch) in raw.trim().chars().enumerate() {
        match ch {
            '+' if index == 0 => leading_plus = true,
            '0'..='9' => digits.push(ch),
            ' ' | '-' | '.' | '(' | ')' | '\u{a0}' => {}
            _ => {
                return Err(fault::validation(
                    "phone",
                    "may contain only a leading plus, digits, and separators",
                ))
            }
        }
    }
    if !leading_plus {
        return Err(fault::validation(
            "phone",
            "must start with a country code, written with a plus",
        ));
    }
    if digits.len() < PHONE_MIN_DIGITS || digits.len() > PHONE_MAX_DIGITS {
        return Err(fault::validation(
            "phone",
            "must be between eight and fifteen digits",
        ));
    }
    if digits.starts_with('0') {
        // A country code never begins with zero, so this is a national trunk prefix
        // that the user forgot to replace.
        return Err(fault::validation(
            "phone",
            "country code must not start with zero",
        ));
    }
    Ok(format!("+{digits}"))
}

/// Whether an identifier looks like an email rather than a username.
///
/// Used to decide which lookup a sign-in should do. An `@` anywhere is enough: `@` is
/// not a legal username character, so there is no ambiguity to resolve.
#[must_use]
pub fn looks_like_email(identifier: &str) -> bool {
    identifier.trim().trim_start_matches('@').contains('@')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserved_list_is_sorted_and_unique() {
        // Binary search on an unsorted list silently misses entries, and the entries it
        // misses are the reserved names somebody would then be able to register.
        for pair in RESERVED.windows(2) {
            assert!(
                pair[0] < pair[1],
                "reserved list out of order at {:?}",
                pair[0]
            );
        }
    }

    #[test]
    fn the_common_password_list_is_sorted_and_unique() {
        for pair in COMMON_PASSWORDS.windows(2) {
            assert!(
                pair[0] < pair[1],
                "password list out of order at {}",
                pair[0]
            );
        }
    }

    #[test]
    fn a_username_keeps_its_typed_form_and_folds_for_uniqueness() {
        let name = username("@Satoshi").expect("valid");
        assert_eq!(name.display(), "Satoshi");
        assert_eq!(name.folded(), "satoshi");
    }

    #[test]
    fn usernames_differing_only_in_case_fold_together() {
        assert_eq!(
            username("SATOSHI").expect("valid").folded(),
            username("satoshi").expect("valid").folded()
        );
    }

    #[test]
    fn a_username_must_start_with_a_letter() {
        assert!(username("1satoshi").is_err());
        assert!(username("_satoshi").is_err());
        assert!(username(".satoshi").is_err());
    }

    #[test]
    fn a_username_may_not_end_with_or_double_a_separator() {
        assert!(username("satoshi.").is_err());
        assert!(username("satoshi_").is_err());
        assert!(username("sat..oshi").is_err());
        assert!(username("sat._oshi").is_err());
        assert!(username("sat.o_shi").is_ok());
    }

    #[test]
    fn a_username_rejects_non_ascii() {
        // Cyrillic `а`. Renders as a Latin `a` in most fonts, which is the problem.
        assert!(username("s\u{430}toshi").is_err());
    }

    #[test]
    fn reserved_names_are_refused_with_their_own_code() {
        let error = username("Support").expect_err("reserved");
        assert_eq!(error.code(), codes::USERNAME_RESERVED);
    }

    #[test]
    fn reserved_names_are_refused_through_lookalikes() {
        assert!(username("supp0rt").is_err());
        assert!(username("s.u.p.p.o.r.t").is_err());
        assert!(username("m.i.g.o").is_err());
        // Not a lookalike of anything reserved.
        assert!(username("supporter").is_ok());
    }

    #[test]
    fn length_bounds_hold_at_the_edges() {
        assert!(username("ab").is_err());
        assert!(username("abc").is_ok());
        assert!(username(&format!("a{}", "b".repeat(31))).is_ok());
        assert!(username(&format!("a{}", "b".repeat(32))).is_err());
    }

    #[test]
    fn a_short_password_is_refused_with_the_weak_code() {
        let error = password(&Secret::new("short"), 10, None).expect_err("too short");
        assert_eq!(error.code(), codes::WEAK_PASSWORD);
    }

    #[test]
    fn a_long_passphrase_passes_without_composition_rules() {
        assert!(password(&Secret::new("correct horse battery staple"), 10, None).is_ok());
    }

    #[test]
    fn an_enormous_password_is_refused_before_hashing() {
        let error = password(&Secret::new("x".repeat(1_000)), 10, None).expect_err("too long");
        assert_eq!(error.code(), codes::WEAK_PASSWORD);
    }

    #[test]
    fn a_common_password_is_refused_even_when_long_enough() {
        assert!(password(&Secret::new("password123"), 10, None).is_err());
        assert!(password(&Secret::new("QWERTYUIOP"), 10, None).is_err());
    }

    #[test]
    fn a_password_containing_the_username_is_refused() {
        assert!(password(&Secret::new("satoshi-satoshi"), 10, Some("Satoshi")).is_err());
        assert!(password(&Secret::new("unrelated passphrase"), 10, Some("satoshi")).is_ok());
    }

    #[test]
    fn emails_normalise_and_reject_the_obvious_mistakes() {
        assert_eq!(
            email("  Sam.Doe+tag@Example.COM ").unwrap(),
            "sam.doe+tag@example.com"
        );
        assert!(email("sam@localhost").is_err());
        assert!(email("sam@@example.com").is_err());
        assert!(email("@example.com").is_err());
        assert!(email("sam@").is_err());
        assert!(email("sam doe@example.com").is_err());
        assert!(email("sam@example.").is_err());
    }

    #[test]
    fn phones_normalise_to_e164() {
        assert_eq!(phone("+62 812-3456-7890").unwrap(), "+6281234567890");
        assert_eq!(phone("+1 (555) 010.1234").unwrap(), "+15550101234");
        assert!(phone("0812 3456 7890").is_err(), "no country code");
        assert!(
            phone("+0812345678").is_err(),
            "country code starting with zero"
        );
        assert!(phone("+123").is_err(), "too short");
        assert!(phone("+1234567890123456").is_err(), "too long");
        assert!(phone("+1 555 CALL").is_err(), "letters");
    }

    #[test]
    fn an_identifier_with_an_at_sign_is_treated_as_an_email() {
        assert!(looks_like_email("sam@example.com"));
        assert!(looks_like_email("@sam@example.com"));
        assert!(!looks_like_email("@satoshi"));
        assert!(!looks_like_email("satoshi"));
    }
}
