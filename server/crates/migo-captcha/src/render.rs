//! The image renderer behind every challenge.
//!
//! A challenge is five to six characters drawn once and thrown away: the answer never
//! reaches the wire, so the picture is the only place it exists. What makes the picture
//! hard for a machine and readable for a person is layered, randomised degradation —
//! per-character rotation, scale, baseline and spacing jitter over three embedded fonts,
//! a noisy ground of dots and speckles, a mild per-row wobble, and one continuous
//! interference curve threaded through the ink of every character — with every parameter
//! drawn from the injected [`Random`], so the same seed reproduces a byte-identical image
//! for a test and two live challenges never share a pixel.
//!
//! The curve is the part worth explaining. It is a Catmull-Rom spline whose knots are
//! placed deliberately: one knot inside the ink band of every character, plus anchors
//! off-canvas on both sides, so the curve is guaranteed to cross all of the text while
//! spanning the full width — never straight, because the per-character knot heights are
//! drawn independently from each character's own band. Making the guarantee structural
//! rather than lucky is what lets a test pin it.
//!
//! The accessible mode ([`RenderParams::accessible`]) is the same machinery with gentler
//! numbers: larger glyphs, smaller rotation, a thinner and fainter curve, a fixed
//! high-contrast ground, and no wobble. It is still an image with a fresh random code —
//! the alternative path is an easier picture, not a bypass.

use std::io::Cursor;

use ab_glyph::{Font, FontRef, Glyph, Point, PxScale};
use image::{DynamicImage, Rgba, RgbaImage};
use migo_core::{Random, Result};
use migo_protocol::fault;

/// The three faces a challenge may draw with. Embedded rather than read from the host,
/// because a deployment container has no fonts, a CI checkout has no fonts, and a challenge
/// that renders differently per machine is a challenge whose difficulty nobody controls.
const FONT_SANS: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Bold.ttf");
const FONT_SERIF: &[u8] = include_bytes!("../assets/fonts/LiberationSerif-Bold.ttf");
const FONT_NARROW: &[u8] = include_bytes!("../assets/fonts/LiberationSansNarrow-Bold.ttf");

/// How much of the canvas a character's ink must leave free on each side, so the answer
/// never kisses the border where a cropping client would eat a stroke.
const MARGIN: u32 = 8;

/// One character as the renderer plans it: its bitmap, where it lands, and the band its
/// ink occupies there. The band is what the interference curve is threaded through.
struct PlacedGlyph {
    /// Rotated, wobbled coverage map: one f32 in `0.0..=1.0` per pixel.
    coverage: Vec<f32>,
    width: usize,
    height: usize,
    /// Left edge on the canvas.
    x: i32,
    /// Top edge on the canvas.
    y: i32,
    /// First and last row (canvas coordinates) carrying ink above the visibility floor.
    ink_top: i32,
    ink_bottom: i32,
    /// Greyscale text colour, jittered per character so a segmentation pass cannot
    /// threshold every glyph with one cut.
    shade: u8,
}

/// The interference curve: a Catmull-Rom spline through these knots, stamped along its
/// length with a soft brush. Knot order is left to right; the first and last sit off-canvas
/// so the curve enters and exits past the borders.
struct Interference {
    knots: Vec<(f32, f32)>,
    thickness: f32,
    alpha: f32,
    shade: u8,
}

/// Everything the painter needs, resolved before a single pixel is touched so tests can
/// inspect the plan without rasterising it.
struct RenderPlan {
    width: u32,
    height: u32,
    background: Rgba<u8>,
    /// Background dots: `(x, y, radius, colour)`, drawn before the text.
    dots: Vec<(f32, f32, f32, Rgba<u8>)>,
    /// Foreground speckles: `(x, y, colour)`, drawn after everything.
    speckles: Vec<(f32, f32, Rgba<u8>)>,
    glyphs: Vec<PlacedGlyph>,
    curve: Interference,
}

/// The knobs a caller holds. Width, height and noise strength come from the deployment's
/// [`migo_core::config::CaptchaConfig`]; `accessible` selects the gentler rendering of the
/// alternative mode.
pub(crate) struct RenderParams {
    pub width: u32,
    pub height: u32,
    /// `1..=5`, the multiplier every noise count scales with.
    pub noise: u8,
    pub accessible: bool,
}

/// Draws `answer` into a PNG. The bytes are the whole deliverable: the caller puts them on
/// the wire base64-encoded and forgets them.
pub(crate) fn render(
    answer: &str,
    params: &RenderParams,
    random: &mut dyn Random,
) -> Result<Vec<u8>> {
    let plan = plan(answer, params, random)?;
    let image = paint(plan);
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| fault::internal(format!("captcha png encode failed: {error}")))?;
    Ok(png)
}

/// The fonts, parsed once per process. `FontRef` is a borrowed view over the static bytes,
/// so holding it costs nothing beyond the parse.
fn fonts() -> Result<&'static [FontRef<'static>; 3]> {
    use std::sync::OnceLock;
    static FONTS: OnceLock<Result<[FontRef<'static>; 3], ()>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            match (
                FontRef::try_from_slice(FONT_SANS),
                FontRef::try_from_slice(FONT_SERIF),
                FontRef::try_from_slice(FONT_NARROW),
            ) {
                (Ok(sans), Ok(serif), Ok(narrow)) => Ok([sans, serif, narrow]),
                _ => Err(()),
            }
        })
        .as_ref()
        .map_err(|()| fault::internal("embedded captcha font is unusable"))
}

// ---------------------------------------------------------------------------
// Randomness helpers. Modulo reduction over a u64 is biased by less than a nanosecond of
// CPU time per call; for aesthetic choices nobody is adversarial over, that is beneath
// the floor of what matters, and the security-relevant randomness (the answer itself)
// goes through the same injected source without any narrowing.
// ---------------------------------------------------------------------------

/// A value in `lo..hi`, exclusive of `hi`.
fn between(random: &mut dyn Random, lo: f32, hi: f32) -> f32 {
    lo + (random.next_u64() as f32 / u64::MAX as f32) * (hi - lo)
}

/// One face of the pool, at random.
fn any_font(random: &mut dyn Random) -> Result<&'static FontRef<'static>> {
    let pool = fonts()?;
    Ok(&pool[(random.next_u64() % pool.len() as u64) as usize])
}

// ---------------------------------------------------------------------------
// Glyph rasterisation, rotation, and wobble
// ---------------------------------------------------------------------------

/// Rasterises `ch` at `scale` pixels of em, returning a coverage bitmap. `None` when the
/// face has no outline for the character, which cannot happen for the challenge alphabet
/// but is the honest answer to the question the type asks.
fn rasterise(font: &FontRef<'_>, ch: char, scale: f32) -> Option<(Vec<f32>, usize, usize)> {
    let glyph = Glyph {
        id: font.glyph_id(ch),
        scale: PxScale::from(scale),
        position: Point { x: 0.0, y: 0.0 },
    };
    let outlined = font.outline_glyph(glyph)?;
    let bounds = outlined.px_bounds();
    let width = (bounds.max.x - bounds.min.x).ceil().max(0.0) as usize;
    let height = (bounds.max.y - bounds.min.y).ceil().max(0.0) as usize;
    if width == 0 || height == 0 {
        return None;
    }
    let mut coverage = vec![0.0f32; width * height];
    outlined.draw(|x, y, value| {
        if let Some(slot) = coverage.get_mut(y as usize * width + x as usize) {
            *slot = value;
        }
    });
    Some((coverage, width, height))
}

/// Rotates a coverage bitmap by `angle` radians around its centre, sampling bilinearly,
/// and applies a per-row horizontal wobble of `wobble` pixels so no glyph keeps a clean
/// vertical stroke. The result's own coverage stays in `0.0..=1.0`.
fn distort(
    coverage: &[f32],
    width: usize,
    height: usize,
    angle: f32,
    wobble: f32,
    phase: f32,
    random: &mut dyn Random,
) -> (Vec<f32>, usize, usize) {
    if width == 0 || height == 0 {
        return (coverage.to_vec(), width, height);
    }
    let (sin, cos) = angle.sin_cos();
    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;
    // The rotated bounding box, in source-pixel units.
    let corners = [(-cx, -cy), (cx, -cy), (cx, cy), (-cx, cy)];
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for (x, y) in corners {
        let rx = x * cos - y * sin;
        let ry = x * sin + y * cos;
        min_x = min_x.min(rx);
        max_x = max_x.max(rx);
        min_y = min_y.min(ry);
        max_y = max_y.max(ry);
    }
    let out_w = ((max_x - min_x).ceil() as usize).max(1);
    let out_h = ((max_y - min_y).ceil() as usize).max(1);
    let mut out = vec![0.0f32; out_w * out_h];
    let frequency = between(random, 0.15, 0.45);
    for dy in 0..out_h {
        for dx in 0..out_w {
            // Destination centre, in rotated-source coordinates.
            let px = dx as f32 + 0.5 - out_w as f32 / 2.0;
            let py = dy as f32 + 0.5 - out_h as f32 / 2.0;
            let sx = px * cos + py * sin + cx;
            let sy = -px * sin + py * cos + cy;
            // The wobble shifts each row horizontally; sampling it into the source lookup
            // is what bends the glyph's vertical strokes.
            let wob = (sy * frequency + phase).sin() * wobble;
            let sx = sx + wob;
            let value = sample(coverage, width, height, sx - 0.5, sy - 0.5);
            out[dy * out_w + dx] = value;
        }
    }
    (out, out_w, out_h)
}

/// Bilinear coverage sample with transparent-black borders.
fn sample(coverage: &[f32], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let clamp = |value: f32, max: usize| value.clamp(0.0, max.saturating_sub(1) as f32);
    let x0 = clamp(x.floor(), width);
    let y0 = clamp(y.floor(), height);
    let x1 = clamp(x.floor() + 1.0, width);
    let y1 = clamp(y.floor() + 1.0, height);
    let fx = (x - x0).clamp(0.0, 1.0);
    let fy = (y - y0).clamp(0.0, 1.0);
    let at = |px: f32, py: f32| coverage[py as usize * width + px as usize];
    let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
    let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Where a glyph's ink starts and ends, vertically, in canvas rows. The visibility floor
/// ignores anti-aliased fringes a human would not read as ink.
fn ink_band(coverage: &[f32], width: usize, height: usize, top: i32) -> (i32, i32) {
    let mut first = height as i32 + top;
    let mut last = top;
    for row in 0..height {
        let row_has_ink = coverage[row * width..(row + 1) * width]
            .iter()
            .any(|value| *value > 0.25);
        if row_has_ink {
            first = first.min(row as i32 + top);
            last = last.max(row as i32 + top);
        }
    }
    (first, last)
}

// ---------------------------------------------------------------------------
// The interference curve
// ---------------------------------------------------------------------------

/// One point of a Catmull-Rom spline segment at parameter `t` in `0..=1`.
fn catmull_rom(
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
    t: f32,
) -> (f32, f32) {
    let t2 = t * t;
    let t3 = t2 * t;
    let m = 0.5;
    let x = m
        * ((2.0 * p1.0)
            + (-p0.0 + p2.0) * t
            + (2.0 * p0.0 - 5.0 * p1.0 + 4.0 * p2.0 - p3.0) * t2
            + (-p0.0 + 3.0 * p1.0 - 3.0 * p2.0 + p3.0) * t3);
    let y = m
        * ((2.0 * p1.1)
            + (-p0.1 + p2.1) * t
            + (2.0 * p0.1 - 5.0 * p1.1 + 4.0 * p2.1 - p3.1) * t2
            + (-p0.1 + 3.0 * p1.1 - 3.0 * p2.1 + p3.1) * t3);
    (x, y)
}

/// Samples the whole spline, segment by segment, densely enough that stamping a disc on
/// every sample draws a continuous stroke.
fn spline_points(knots: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut points = Vec::new();
    if knots.len() < 2 {
        return points;
    }
    for index in 0..knots.len() - 1 {
        let p0 = knots[index.saturating_sub(1)];
        let p1 = knots[index];
        let p2 = knots[index + 1];
        let p3 = knots[(index + 2).min(knots.len() - 1)];
        let steps = 48;
        for step in 0..=steps {
            points.push(catmull_rom(p0, p1, p2, p3, step as f32 / steps as f32));
        }
    }
    points
}

/// The curve's knots: one per character, inside that character's ink band, plus off-canvas
/// anchors on both sides. Public to this crate's tests because the guarantee it encodes —
/// the curve crosses every character — is structural, and the test pins the structure.
fn interference_knots(
    glyphs: &[PlacedGlyph],
    width: u32,
    random: &mut dyn Random,
) -> Vec<(f32, f32)> {
    let mut knots = Vec::with_capacity(glyphs.len() + 2);
    knots.push((-15.0, 0.0));
    for glyph in glyphs {
        let centre = glyph.x as f32 + glyph.width as f32 / 2.0;
        let band_top = glyph.ink_top as f32;
        let band_height = (glyph.ink_bottom - glyph.ink_top).max(1) as f32;
        let y = between(
            random,
            band_top + band_height * 0.15,
            band_top + band_height * 0.85,
        );
        knots.push((centre, y));
    }
    knots.push((width as f32 + 15.0, 0.0));
    // The anchors' heights are drawn from the mean of the character knots so the curve
    // still enters and exits the canvas inside the text's vertical range — a curve that
    // dove off the top border and back would read as decoration, not interference.
    let mid = knots
        .iter()
        .skip(1)
        .take(knots.len().saturating_sub(2))
        .map(|(_, y)| *y)
        .sum::<f32>()
        / (knots.len().saturating_sub(2)).max(1) as f32;
    if let Some(first) = knots.first_mut() {
        first.1 = mid + between(random, -10.0, 10.0);
    }
    if let Some(last) = knots.last_mut() {
        last.1 = mid + between(random, -10.0, 10.0);
    }
    knots
}

// ---------------------------------------------------------------------------
// Planning and painting
// ---------------------------------------------------------------------------

fn plan(answer: &str, params: &RenderParams, random: &mut dyn Random) -> Result<RenderPlan> {
    let width = params.width.max(120);
    let height = params.height.max(64);
    let accessible = params.accessible;

    // Character sizes: larger and tighter-ranged in the accessible mode, where the point
    // is legibility, and varied enough in the standard mode that no two glyphs of one
    // challenge look like they came from the same stamp.
    let size_lo = if accessible { 46.0 } else { 38.0 };
    let size_hi = if accessible { 54.0 } else { 48.0 };
    let angle_range = if accessible { 10.0 } else { 20.0 };
    let wobble = if accessible { 0.0 } else { 2.0 };

    let mut glyphs: Vec<PlacedGlyph> = Vec::with_capacity(answer.chars().count());
    for ch in answer.chars() {
        let font = any_font(random)?;
        let scale = between(random, size_lo, size_hi);
        let Some((coverage, glyph_w, glyph_h)) = rasterise(font, ch, scale) else {
            // The alphabet is closed and every face covers it; an uncovered character is a
            // bug in the embedded assets, not a runtime condition to quietly skip.
            return Err(fault::internal(format!(
                "captcha font has no outline for {ch:?}"
            )));
        };
        let angle = between(random, -angle_range, angle_range).to_radians();
        let phase = between(random, 0.0, std::f32::consts::TAU);
        let (coverage, glyph_w, glyph_h) =
            distort(&coverage, glyph_w, glyph_h, angle, wobble, phase, random);
        // Vertical placement: centred with jitter, clamped so the whole glyph is on the
        // canvas with a little air. The jitter is the "baseline" randomisation — a fixed
        // baseline is the alignment OCR relies on first.
        let jitter = if accessible { 2.0 } else { 5.0 };
        let y = (between(random, -jitter, jitter) + (height as i32 - glyph_h as i32) as f32 / 2.0)
            .round() as i32;
        let y = y.clamp(3, (height as i32 - glyph_h as i32 - 3).max(3));
        let shade = if accessible {
            15
        } else {
            (between(random, 15.0, 70.0)) as u8
        };
        let (ink_top, ink_bottom) = ink_band(&coverage, glyph_w, glyph_h, y);
        glyphs.push(PlacedGlyph {
            coverage,
            width: glyph_w,
            height: glyph_h,
            x: 0,
            y,
            ink_top,
            ink_bottom,
            shade,
        });
    }

    // Horizontal layout: jittered gaps, centred, and shrunk to fit by tightening the gaps
    // before ever touching the glyphs, because a size change re-rasterises and a spacing
    // change is free.
    let total_glyph_width: usize = glyphs.iter().map(|glyph| glyph.width).sum();
    let gaps = glyphs.len().saturating_sub(1);
    let available = (width as usize).saturating_sub(2 * MARGIN as usize);
    let gap_lo = if accessible { 4.0 } else { 2.0 };
    let gap_hi = if accessible { 7.0 } else { 11.0 };
    let mut spacing: Vec<i32> = (0..gaps)
        .map(|_| between(random, gap_lo, gap_hi) as i32)
        .collect();
    let mut total = total_glyph_width + spacing.iter().sum::<i32>() as usize;
    if total > available {
        // Tighten every gap proportionally, then floor them at two pixels; if even that
        // overflows the glyphs are simply wider than the canvas allows for this many
        // characters, which the configuration validator's bounds keep out of reach.
        let excess = total - available;
        let current: i32 = spacing.iter().sum();
        if current > 0 {
            let factor = (current as f32 - excess as f32).max(0.0) / current as f32;
            for gap in &mut spacing {
                *gap = (*gap as f32 * factor) as i32;
            }
        }
        total = total_glyph_width + spacing.iter().sum::<i32>() as usize;
        let _ = total;
    }
    let used = total_glyph_width + spacing.iter().sum::<i32>() as usize;
    let mut x = ((width as usize - used) as f32 / 2.0).round() as i32;
    for (index, glyph) in glyphs.iter_mut().enumerate() {
        glyph.x = x;
        x += glyph.width as i32 + spacing.get(index).copied().unwrap_or(0);
    }

    // The ground: a light, near-neutral tint in the standard mode (fixed near-white in the
    // accessible one), then a field of low-opacity dots under the text and a scatter of
    // speckles above everything. Counts scale with the configured strength.
    let background = if accessible {
        Rgba([252, 252, 252, 255])
    } else {
        let level = between(random, 240.0, 250.0) as u8;
        let tint = between(random, -6.0, 6.0) as i16;
        let channel = |base: u8| (base as i16 + tint).clamp(0, 255) as u8;
        Rgba([channel(level), channel(level), channel(level), 255])
    };
    let strength = params.noise.max(1) as i32;
    let dot_count = strength * 50;
    let speckle_count = strength * 90;
    let mut dots = Vec::with_capacity(dot_count as usize);
    for _ in 0..dot_count {
        let x = between(random, 0.0, width as f32);
        let y = between(random, 0.0, height as f32);
        let radius = between(random, 0.7, 2.2);
        let level = between(random, 90.0, 200.0) as u8;
        let alpha = between(random, 26.0, 70.0) as u8;
        dots.push((x, y, radius, Rgba([level, level, level, alpha])));
    }
    let mut speckles = Vec::with_capacity(speckle_count as usize);
    for _ in 0..speckle_count {
        let x = between(random, 0.0, width as f32);
        let y = between(random, 0.0, height as f32);
        let level = between(random, 60.0, 190.0) as u8;
        let alpha = between(random, 30.0, 80.0) as u8;
        speckles.push((x, y, Rgba([level, level, level, alpha])));
    }

    let knots = interference_knots(&glyphs, width, random);
    let curve = Interference {
        knots,
        thickness: if accessible {
            between(random, 1.0, 2.0)
        } else {
            between(random, 2.0, 4.0)
        },
        alpha: if accessible {
            0.35
        } else {
            between(random, 0.5, 0.8)
        },
        shade: if accessible {
            100
        } else {
            between(random, 55.0, 95.0) as u8
        },
    };

    Ok(RenderPlan {
        width,
        height,
        background,
        dots,
        speckles,
        glyphs,
        curve,
    })
}

/// Blends `source` over the canvas at `(x, y)` with `source`'s own alpha.
fn blend(canvas: &mut RgbaImage, x: i32, y: i32, source: Rgba<u8>) {
    if x < 0 || y < 0 || x >= canvas.width() as i32 || y >= canvas.height() as i32 {
        return;
    }
    let pixel = canvas.get_pixel_mut(x as u32, y as u32);
    let alpha = f32::from(source[3]) / 255.0;
    for channel in 0..3 {
        let over = f32::from(source[channel]) * alpha;
        let under = f32::from(pixel[channel]) * (1.0 - alpha);
        pixel[channel] = (over + under).round().clamp(0.0, 255.0) as u8;
    }
}

/// Fills a soft disc: full coverage at the centre, falling to zero at the rim.
fn stamp(canvas: &mut RgbaImage, x: f32, y: f32, radius: f32, colour: Rgba<u8>) {
    let reach = radius.ceil() as i32;
    let centre_x = x.round() as i32;
    let centre_y = y.round() as i32;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let distance = ((dx * dx + dy * dy) as f32).sqrt();
            if distance <= radius {
                let mut faded = colour;
                let edge = (1.0 - distance / radius).clamp(0.0, 1.0);
                faded[3] = (f32::from(faded[3]) * edge).round().clamp(0.0, 255.0) as u8;
                blend(canvas, centre_x + dx, centre_y + dy, faded);
            }
        }
    }
}

fn paint(plan: RenderPlan) -> RgbaImage {
    let mut canvas = RgbaImage::from_pixel(plan.width, plan.height, plan.background);

    for (x, y, radius, colour) in &plan.dots {
        stamp(&mut canvas, *x, *y, *radius, *colour);
    }

    for glyph in &plan.glyphs {
        for row in 0..glyph.height {
            for column in 0..glyph.width {
                let coverage = glyph.coverage[row * glyph.width + column];
                if coverage <= 0.02 {
                    continue;
                }
                let alpha = (coverage * 255.0).round().clamp(0.0, 255.0) as u8;
                blend(
                    &mut canvas,
                    glyph.x + column as i32,
                    glyph.y + row as i32,
                    Rgba([glyph.shade, glyph.shade, glyph.shade, alpha]),
                );
            }
        }
    }

    let curve_colour = Rgba([
        plan.curve.shade,
        plan.curve.shade,
        plan.curve.shade,
        (plan.curve.alpha * 255.0).round().clamp(0.0, 255.0) as u8,
    ]);
    for (x, y) in spline_points(&plan.curve.knots) {
        stamp(&mut canvas, x, y, plan.curve.thickness, curve_colour);
    }

    for (x, y, colour) in &plan.speckles {
        blend(&mut canvas, x.round() as i32, y.round() as i32, *colour);
    }

    canvas
}
