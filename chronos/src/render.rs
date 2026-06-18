//! Glyph-based clock rendering into a 32-bit ARGB framebuffer.
//!
//! Renders a large two-line clock (HH over MM) in Rubik Light, anti-aliased,
//! for a clean modern look à la the Pixel screensaver. The hours are dimmed to
//! a soft grey and the minutes stay white, giving a quiet visual hierarchy.
//! Rubik is bundled (OFL, see `assets/LICENSE-Rubik.txt`).

use fontdue::Font;

/// Bundled Rubik Light — gently rounded corners, clean and modern.
const FONT_DATA: &[u8] = include_bytes!("../assets/Rubik-Light.ttf");

/// Load the clock font. `CHRONOS_FONT` overrides the bundled file (lets us
/// audition families on a kiosk without a rebuild); production uses the bundle.
pub fn load_font() -> Font {
    if let Ok(path) = std::env::var("CHRONOS_FONT") {
        match std::fs::read(&path) {
            Ok(bytes) => match Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                Ok(f) => {
                    tracing::info!("loaded font override from {path}");
                    return f;
                }
                Err(e) => tracing::error!("CHRONOS_FONT {path} failed to parse ({e}); using bundled"),
            },
            Err(e) => tracing::error!("CHRONOS_FONT {path} unreadable ({e}); using bundled"),
        }
    }
    Font::from_bytes(FONT_DATA, fontdue::FontSettings::default())
        .expect("bundled font failed to parse")
}

/// Scale each RGB channel of an ARGB8888 colour by `factor` (alpha kept opaque).
fn dim(colour: u32, factor: f32) -> u32 {
    let ch = |shift: u32| {
        let v = ((colour >> shift) & 0xff) as f32 * factor;
        (v.round().clamp(0.0, 255.0) as u32) << shift
    };
    0xFF00_0000 | ch(16) | ch(8) | ch(0)
}

/// ARGB8888 canvas (0xAARRGGBB per pixel, matching little-endian wl_shm Argb8888).
pub struct Canvas<'a> {
    pub buf: &'a mut [u32],
    pub width: i32,
    pub height: i32,
}

impl<'a> Canvas<'a> {
    fn fill(&mut self, colour: u32) {
        self.buf.iter_mut().for_each(|p| *p = colour);
    }

    /// Alpha-composite a coverage bitmap (`cov`, 0-255) of size `gw`×`gh` at
    /// top-left (`ox`, `oy`), blending `fg` over `bg`.
    #[allow(clippy::too_many_arguments)]
    fn blit(&mut self, cov: &[u8], gw: usize, gh: usize, ox: i32, oy: i32, fg: u32, bg: u32) {
        let (fr, fg_, fb) = ((fg >> 16) & 0xff, (fg >> 8) & 0xff, fg & 0xff);
        let (br, bg_, bb) = ((bg >> 16) & 0xff, (bg >> 8) & 0xff, bg & 0xff);
        for gy in 0..gh {
            let py = oy + gy as i32;
            if py < 0 || py >= self.height {
                continue;
            }
            let row = (py * self.width) as usize;
            for gx in 0..gw {
                let px = ox + gx as i32;
                if px < 0 || px >= self.width {
                    continue;
                }
                let a = cov[gy * gw + gx] as u32;
                if a == 0 {
                    continue;
                }
                let mix = |f: u32, b: u32| (b * (255 - a) + f * a) / 255;
                let r = mix(fr, br);
                let g = mix(fg_, bg_);
                let b = mix(fb, bb);
                self.buf[row + px as usize] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
            }
        }
    }
}

/// Lay out a centred line of text at pixel size `px`, returning the glyph
/// rasters and the line's total advance width / max cap height.
fn rasterize_line(font: &Font, text: &str, px: f32) -> (Vec<(fontdue::Metrics, Vec<u8>)>, f32, f32) {
    let glyphs: Vec<(fontdue::Metrics, Vec<u8>)> =
        text.chars().map(|c| font.rasterize(c, px)).collect();
    let advance: f32 = glyphs.iter().map(|(m, _)| m.advance_width).sum();
    // Use the digit cap height (tallest glyph) for vertical centring.
    let cap = glyphs.iter().map(|(m, _)| m.height).max().unwrap_or(0) as f32;
    (glyphs, advance, cap)
}

fn draw_line(canvas: &mut Canvas, glyphs: &[(fontdue::Metrics, Vec<u8>)], advance: f32, cx: f32, baseline_y: f32, fg: u32, bg: u32) {
    let mut pen_x = cx - advance / 2.0;
    for (m, bmp) in glyphs {
        // fontdue: bitmap bottom sits at baseline - ymin; top = baseline - (height + ymin).
        let gx = (pen_x + m.xmin as f32).round() as i32;
        let gy = (baseline_y - (m.height as f32 + m.ymin as f32)).round() as i32;
        canvas.blit(bmp, m.width, m.height, gx, gy, fg, bg);
        pen_x += m.advance_width;
    }
}

/// Render HH (top) over MM (bottom), centred. `fg`/`bg` are ARGB8888; the hours
/// are dimmed to a soft grey (CHRONOS_HOUR_DIM overrides the factor).
pub fn render_clock(canvas: &mut Canvas, font: &Font, hh: u8, mm: u8, fg: u32, bg: u32) {
    canvas.fill(bg);

    let w = canvas.width as f32;
    let h = canvas.height as f32;

    // Pick a glyph size that fills the panel nicely in both axes.
    let px = (h * 0.34).min(w * 0.70);

    let hour_dim = std::env::var("CHRONOS_HOUR_DIM")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.65);
    let hour_fg = dim(fg, hour_dim);

    let (top, top_adv, cap) = rasterize_line(font, &format!("{hh:02}"), px);
    let (bot, bot_adv, _) = rasterize_line(font, &format!("{mm:02}"), px);

    // Stack the two lines and centre the block vertically. The gap is a
    // fraction of the cap height; CHRONOS_GAP overrides it for live tuning.
    let gap_factor = std::env::var("CHRONOS_GAP")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.55);
    let gap = cap * gap_factor;
    let block_h = 2.0 * cap + gap;
    let top_baseline = (h - block_h) / 2.0 + cap;
    let bot_baseline = top_baseline + cap + gap;
    let cx = w / 2.0;

    draw_line(canvas, &top, top_adv, cx, top_baseline, hour_fg, bg);
    draw_line(canvas, &bot, bot_adv, cx, bot_baseline, fg, bg);
}
