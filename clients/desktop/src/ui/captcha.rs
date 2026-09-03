//! The image captcha on the register and sign-in forms.
//!
//! # Why a fetch needs a flag
//!
//! egui calls a form function every frame, so "nothing held? ask for one", written where the
//! picture is drawn, would fire a request per frame until one arrived. The fetch is
//! edge-triggered instead: it fires on the transition into "nothing held, nothing in flight,
//! nothing failed", and the in-flight flag is what stops the still-true condition from firing
//! twice. A failed fetch deliberately does not retry on its own — an unreachable server would
//! turn that into the same storm — so the retry is the user's click.
//!
//! # The life of a challenge
//!
//! A challenge is one-shot on the server: the first proof against it consumes it, right or
//! wrong. So the held challenge leaves with the submit that carried its proof, cleared answer
//! and all — an answer typed against a dead challenge can only be wrong — and the next frame
//! fetches a fresh one. A refusal from the server (wrong, expired, or missing proof) clears it
//! the same way, but the form stays standing either way, ready for another attempt.

use egui::{RichText, Ui};

use crate::config::ServerEndpoint;
use crate::net::rest::CaptchaChallenge;
use crate::net::{CaptchaAnswer, Command};
use crate::theme::{font, palette, space, text_style, Theme};
use crate::ui::{widgets, Context};

/// The accessible rendering mode, spelled as the wire spells it.
///
/// The one mode string this module ever sends: the standard mode is never named, it is simply
/// the absence of a request, which lets the server pick its own default.
const MODE_IMAGE_ALT: &str = "image_alt";

/// The captcha half of a form: the challenge being shown, the answer typed against it, and the
/// state of the fetch that produces the next one.
#[derive(Default)]
pub struct CaptchaState {
    /// The id of the held challenge, or `None` when nothing is held. The id is the only part the
    /// wire needs back, and it doubles as the flag the fetch trigger reads — so it, not the
    /// pixels, is the field that says whether a challenge exists at all.
    challenge_id: Option<String>,
    /// The rendering mode to ask for next: the echo of the held challenge, or the last mode
    /// asked for when nothing is held. `None` until the user expresses a preference.
    mode: Option<String>,
    /// The decoded pixels of the held challenge. Kept alongside the texture because the texture
    /// is cheap to rebuild from pixels and the pixels are expensive to re-decode from base64 —
    /// and the rebuild happens once per challenge, not once per frame.
    image: Option<egui::ColorImage>,
    /// The uploaded texture of the held challenge. Dropped with the challenge it shows, so each
    /// challenge uploads exactly one texture.
    texture: Option<egui::TextureHandle>,
    /// What the user has read off the image so far.
    answer: String,
    /// True from the fetch command to the event that answers it. This is the flag that turns
    /// egui's per-frame redraws back into one request per want.
    fetching: bool,
    /// Why the last fetch failed, when it did. Held so the failure can be drawn in place of the
    /// image, and so the trigger stays quiet until the user asks again.
    failure: Option<String>,
}

impl CaptchaState {
    /// Absorbs a fetched challenge: decode it, hold it, stop claiming to be in flight.
    ///
    /// A challenge whose picture will not decode is treated as no challenge at all — holding
    /// its id would let a submit send a proof for an image nobody ever saw.
    pub fn hold(&mut self, challenge: CaptchaChallenge) {
        match captcha_image(&challenge.image_png_base64) {
            Some(image) => {
                self.challenge_id = Some(challenge.challenge_id);
                self.mode = Some(challenge.mode);
                self.image = Some(image);
                self.texture = None;
                self.failure = None;
                // The picture changed, so whatever was typed belongs to it: a replacement
                // that arrives with a refusal swaps the challenge out from under an answer
                // that may still be on screen, and carrying it into the retry would submit
                // the old reading against the new question.
                self.answer.clear();
            }
            None => {
                self.forget();
                self.failure = Some("the server sent an unreadable challenge".to_owned());
            }
        }
        self.fetching = false;
    }

    /// Records a fetch that could not produce a challenge.
    ///
    /// `reason` is a public message — either this client's own words or the server's — so the
    /// form can show it verbatim. No automatic retry: see the module docs.
    pub fn unavailable(&mut self, reason: String) {
        self.fetching = false;
        self.failure = Some(reason);
    }

    /// Drops the held challenge so the next frame fetches a fresh one.
    ///
    /// Called when the server refuses a submit with a captcha error: the proof consumed the
    /// challenge whether or not it was accepted, so nothing held is worth keeping. The form
    /// itself is untouched — this changes the picture, not the screen.
    pub fn refused(&mut self) {
        self.forget();
        self.failure = None;
    }

    /// Takes the proof a submit should carry, consuming the challenge it answers.
    ///
    /// `None` when there is nothing worth sending: no challenge held, or an answer that is not
    /// five or six letters and digits once normalised. In that case the challenge is *not*
    /// consumed — no proof reached the server, so it never saw it — and stays held for the next
    /// click.
    pub fn take_proof(&mut self) -> Option<CaptchaAnswer> {
        let challenge_id = self.challenge_id.clone()?;
        let answer = normalised(&self.answer);
        if !(5..=6).contains(&answer.len()) || !answer.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        self.forget();
        Some(CaptchaAnswer {
            challenge_id,
            answer,
        })
    }

    /// Forgets everything about a server: its challenge, its mode, the answer typed against it.
    ///
    /// A challenge issued by one server cannot be answered on another, so a changed endpoint
    /// leaves nothing worth holding.
    pub fn reset(&mut self) {
        self.forget();
        self.mode = None;
        self.failure = None;
    }

    /// Drops the held challenge: id, image, texture, and the answer typed against it.
    ///
    /// The mode deliberately survives — someone who asked for the easier rendering wants the
    /// easier rendering again — and so do the fetch flags, which describe a request that may
    /// still be in flight.
    fn forget(&mut self) {
        self.challenge_id = None;
        self.image = None;
        self.texture = None;
        self.answer.clear();
    }
}

/// Draws the captcha section of a form, and issues its fetches.
///
/// One function owns both halves — the picture and the request that produces it — because
/// "this section is on screen holding nothing" is precisely the fetch condition. Keeping them
/// together is what makes it impossible for a form to draw the section without also feeding it.
pub fn show(
    ui: &mut Ui,
    context: &mut Context<'_>,
    captcha: &mut CaptchaState,
    server: &ServerEndpoint,
) {
    let theme = context.theme;
    let colors = palette(theme);

    // The edge trigger: one fetch per transition into "nothing held, nothing in flight, nothing
    // failed". A redraw in that state — or a hundred — still sends exactly one request, because
    // the flag set here is only cleared by the event that answers it.
    if captcha.challenge_id.is_none() && !captcha.fetching && captcha.failure.is_none() {
        captcha.fetching = true;
        context.issue(Command::FetchCaptcha {
            server: server.clone(),
            mode: captcha.mode.clone(),
        });
    }

    ui.label(
        RichText::new("Human check")
            .text_style(crate::theme::named(text_style::OVERLINE))
            .color(colors.text_muted),
    );
    ui.add_space(space::XS);

    if let Some(reason) = captcha.failure.as_ref() {
        problem(
            ui,
            theme,
            &format!("Could not load the challenge: {reason}."),
        );
        ui.add_space(space::XS);
        if widgets::ghost_button(ui, theme, "Try again").clicked() {
            captcha.failure = None;
            captcha.fetching = true;
            context.issue(Command::FetchCaptcha {
                server: server.clone(),
                mode: captcha.mode.clone(),
            });
        }
    } else if let Some(image) = captcha.image.as_ref() {
        // One upload per challenge: the handle is created the frame the challenge arrives and
        // dropped with it. Loading under a stable name replaces the previous challenge's upload,
        // so the old picture's memory does not sit around waiting for egui's texture GC.
        let texture = captcha.texture.get_or_insert_with(|| {
            ui.ctx().load_texture(
                "auth-captcha",
                image.clone(),
                egui::TextureOptions::default(),
            )
        });
        // Scaled to the column width with the aspect kept, so the glyphs grow with the window
        // rather than a 260x96 picture squatting inside a wider form.
        let width = ui.available_width();
        let scale = width / image.size[0] as f32;
        let height = image.size[1] as f32 * scale;
        ui.image((texture.id(), egui::vec2(width, height)));

        ui.add_space(space::SM);
        // Same field anatomy as [`widgets::field`], drawn here rather than delegated because the
        // label belongs to the section (it shows while loading and on failure too), and a field
        // without its own label is not a shape anything else in the window needs.
        ui.add(
            egui::TextEdit::singleline(&mut captcha.answer)
                .hint_text("5–6 characters")
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8)),
        );

        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            if widgets::ghost_button(ui, theme, "New challenge").clicked() {
                captcha.forget();
                captcha.fetching = true;
                context.issue(Command::FetchCaptcha {
                    server: server.clone(),
                    mode: captcha.mode.clone(),
                });
            }
            ui.add_space(space::XS);
            // The accessible path: a gentler rendering of a *new* code, never the same code made
            // legible, because the alternative must remain a captcha rather than a bypass.
            if widgets::ghost_button(ui, theme, "Easier challenge")
                .on_hover_text("A clearer rendering, with a new code")
                .clicked()
            {
                captcha.forget();
                captcha.mode = Some(MODE_IMAGE_ALT.to_owned());
                captcha.fetching = true;
                context.issue(Command::FetchCaptcha {
                    server: server.clone(),
                    mode: captcha.mode.clone(),
                });
            }
        });
    } else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.add_space(space::XS);
            ui.label(
                RichText::new("getting a challenge…")
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
            );
        });
    }
    ui.add_space(space::MD);
}

/// A failure in the danger colour.
///
/// The same treatment as the auth forms' own `problem`, kept local because it is three lines and
/// this section's failures are its own concern.
fn problem(ui: &mut Ui, theme: Theme, text: &str) {
    let colors = palette(theme);
    ui.label(
        RichText::new(text)
            .font(egui::FontId::proportional(font::SMALL))
            .color(colors.danger),
    );
}

/// The form of an answer worth sending: upper-cased, whitespace-free.
///
/// The server does exactly this before comparing, so doing it here is a courtesy — but it means
/// the length check judges what the server will judge, and a user whose keyboard capitalises
/// for them is never refused for it.
fn normalised(answer: &str) -> String {
    answer
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_uppercase()
}

/// Decodes a wire challenge into pixels egui can upload.
///
/// Total on purpose: bad base64, a truncated PNG, an image the decoder rejects — every failure
/// is `None`, never a panic, because this runs on the paint loop and a panic there takes the
/// whole window down. The caller decides what the absence of a picture looks like.
pub fn captcha_image(png_base64: &str) -> Option<egui::ColorImage> {
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD
        .decode(png_base64.as_bytes())
        .ok()?;
    let rgba = image::load_from_memory(&png).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        rgba.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A real round trip: the PNG is encoded here with the same crate that decodes it, so the
    /// test exercises the whole path — base64, PNG, pixels — with nothing mocked.
    #[test]
    fn a_valid_png_decodes_to_its_own_dimensions() {
        let pixels = image::ImageBuffer::from_pixel(2, 2, image::Rgba([0u8, 0, 0, 255]));
        let mut png = Vec::new();
        pixels
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encoding a 2x2 PNG cannot fail");
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let decoded = captcha_image(&encoded).expect("a valid PNG decodes");
        assert_eq!(decoded.size, [2, 2]);
    }

    /// Neither garbage nor emptiness may panic: this runs on the paint loop, where a panic takes
    /// the window with it. Both are simply "no image".
    #[test]
    fn garbage_and_empty_input_are_none_rather_than_panics() {
        assert!(captcha_image("not-base64!!").is_none());
        assert!(captcha_image("").is_none());
    }

    /// The proof gate: normalisation happens before the shape is judged, so whitespace and case
    /// never fail a correct read, and anything that is not five or six letters and digits stays
    /// behind.
    #[test]
    fn the_proof_gate_judges_the_normalised_answer() {
        let mut captcha = CaptchaState::default();
        assert!(
            captcha.take_proof().is_none(),
            "no challenge held, so no proof"
        );

        captcha.challenge_id = Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned());
        captcha.answer = "  a b\t3d7 ".to_owned();
        let proof = captcha.take_proof().expect("a correct read is a proof");
        assert_eq!(proof.challenge_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(proof.answer, "AB3D7");
        assert!(
            captcha.take_proof().is_none(),
            "the challenge left with the proof that consumed it"
        );

        captcha.challenge_id = Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned());
        captcha.answer = "AB3".to_owned();
        assert!(captcha.take_proof().is_none(), "too short to send");
        assert!(
            captcha.challenge_id.is_some(),
            "an unsent proof does not consume the challenge"
        );

        captcha.answer = "AB3D7IOS".to_owned();
        assert!(captcha.take_proof().is_none(), "too long to send");
    }

    /// A refusal's replacement lands exactly like a fetched one — and the part worth pinning is
    /// what it does to the answer already on screen: the picture changed, so the reading typed
    /// against the old one is void, not carried into the retry.
    #[test]
    fn a_replacement_challenge_voids_the_answer_it_interrupted() {
        let pixels = image::ImageBuffer::from_pixel(2, 2, image::Rgba([0u8, 0, 0, 255]));
        let mut png = Vec::new();
        pixels
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encoding a 2x2 PNG cannot fail");
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);

        let mut captcha = CaptchaState::default();
        captcha.hold(challenge("01ARZ3NDEKTSV4RRFFQ69G5FAV", &encoded));
        captcha.answer = "  a b\t3d7 ".to_owned();

        captcha.hold(challenge("01ARZ3NDEKTSV4RRFFQ69G5FAW", &encoded));
        assert_eq!(
            captcha.challenge_id.as_deref(),
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAW"),
            "the replacement is the held challenge now"
        );
        assert_eq!(captcha.answer, "", "the old picture's answer is void");
        assert!(
            captcha.take_proof().is_none(),
            "no proof exists until the new picture is read"
        );
    }

    /// A challenge as the server issues it, around an encoded picture the tests share.
    fn challenge(id: &str, image_png_base64: &str) -> CaptchaChallenge {
        CaptchaChallenge {
            challenge_id: id.to_owned(),
            image_png_base64: image_png_base64.to_owned(),
            mode: "image".to_owned(),
            ttl_seconds: 120,
        }
    }
}
