//! Crop the avatar before saving: the picked image in a square viewport with a round frame.
//! Drag to move it, zoom with the slider or the scroll wheel, and see the result as the round
//! avatar at the sizes kall uses. Apply makes the 128×128 PNG that Save uploads.

use anyhow::{Result, anyhow};
use rune::tok::type_;
use rune::{El, Phase, TextStyle, col, el, oklch, oklcha, row, text};

use super::{button, glyph, pal};
use crate::{State, icons};

/// The picked image and how it's framed.
pub struct Crop {
    pub px: rune::Pixels,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    /// 1 shows the largest square that fits; up to [`MAX_ZOOM`].
    pub zoom: f32,
    /// The frame's centre, in image pixels.
    pub cx: f32,
    pub cy: f32,
    /// The centre when a drag started.
    grab: (f32, f32),
}

pub const MAX_ZOOM: f32 = 5.0;
const VIEW: f32 = 320.0;
/// Avatars are stored at this size.
pub const OUT: u32 = 128;

impl Crop {
    pub fn new(w: u32, h: u32, rgba: Vec<u8>) -> Crop {
        let px = rune::Pixels::new();
        px.set(w, h, rgba.clone());
        Crop { px, w, h, rgba, zoom: 1.0, cx: w as f32 / 2.0, cy: h as f32 / 2.0, grab: (0.0, 0.0) }
    }

    /// The side of the framed square, in image pixels.
    pub fn side(&self) -> f32 {
        self.w.min(self.h) as f32 / self.zoom
    }

    /// Keep the frame inside the image.
    pub fn clamp(&mut self) {
        self.zoom = self.zoom.clamp(1.0, MAX_ZOOM);
        let half = self.side() / 2.0;
        self.cx = self.cx.clamp(half, self.w as f32 - half);
        self.cy = self.cy.clamp(half, self.h as f32 - half);
    }

    /// The framed square: (left, top, side) in whole image pixels, inside the image.
    pub fn rect(&self) -> (u32, u32, u32) {
        let side = (self.side().round() as u32).clamp(1, self.w.min(self.h));
        let x = ((self.cx - side as f32 / 2.0).round().max(0.0) as u32).min(self.w - side);
        let y = ((self.cy - side as f32 / 2.0).round().max(0.0) as u32).min(self.h - side);
        (x, y, side)
    }

    /// The framed square as the avatar PNG.
    pub fn png(&self) -> Result<Vec<u8>> {
        let img = image::RgbaImage::from_raw(self.w, self.h, self.rgba.clone()).ok_or_else(|| anyhow!("bad image"))?;
        let (x, y, side) = self.rect();
        let cut = image::imageops::crop_imm(&img, x, y, side, side).to_image();
        let small = image::imageops::resize(&cut, OUT, OUT, image::imageops::FilterType::Lanczos3);
        let mut out = std::io::Cursor::new(Vec::new());
        small.write_to(&mut out, image::ImageFormat::Png)?;
        Ok(out.into_inner())
    }
}

fn crop(s: &mut State) -> Option<&mut Crop> {
    s.settings.as_mut()?.crop.as_mut()
}

/// The image as framed, drawn into a `size`-point square (clipped round when `round`).
fn framed(c: &Crop, size: f32, round: bool) -> El {
    let scale = size / c.side();
    let (iw, ih) = (c.w as f32 * scale, c.h as f32 * scale);
    let (x, y) = (size / 2.0 - c.cx * scale, size / 2.0 - c.cy * scale);
    let r = if round { size / 2.0 } else { 12.0 };
    el().size(size, size).radius(r).clip_rounded(r).fill(oklch(0.1, 0.0, 0.0)).child(el().size(iw, ih).absolute(x, y).pixels(&c.px))
}

/// A slider track (0–1) that calls `set` while you click or drag it.
fn slider(key: &'static str, frac: f32, set: fn(&mut State, f32)) -> El {
    let p = pal();
    el().key(key)
        .role("slider")
        .label("Zoom")
        .grow(1.0)
        .h(24.0)
        .child(el().pin(Some(0.0), Some(10.0), Some(0.0), None).h(4.0).radius(2.0).fill(p.ink(0.15)))
        .child(el().pin(Some(0.0), Some(10.0), None, None).h(4.0).radius(2.0).w_frac(frac).fill(p.accent))
        .child(
            row()
                .pin(Some(0.0), Some(4.0), Some(0.0), None)
                .h(16.0)
                .child(el().grow(frac))
                .child(el().size(16.0, 16.0).radius(8.0).fill(oklch(0.98, 0.0, 0.0)).border(1.0, p.ink(0.2)))
                .child(el().grow(1.0 - frac)),
        )
        .on_drag(move |s: &mut State, d, _| {
            if d.w > 0.0 {
                set(s, (d.x / d.w).clamp(0.0, 1.0));
            }
        })
}

/// The crop dialog, over Settings.
pub fn view(c: &Crop) -> El {
    let p = pal();
    // The image, a darkened outside and a white ring: the circle is what becomes the avatar.
    let shade = VIEW;
    let viewport = framed(c, VIEW, false)
        .key("crop-view")
        .role("img")
        .label("Drag to move, scroll to zoom")
        .child(el().size(VIEW + 2.0 * shade, VIEW + 2.0 * shade).absolute(-shade, -shade).radius(VIEW / 2.0 + shade).border(shade, oklcha(0.0, 0.0, 0.0, 0.55)))
        .child(el().size(VIEW, VIEW).absolute(0.0, 0.0).radius(VIEW / 2.0).border(2.0, oklcha(1.0, 0.0, 0.0, 0.85)))
        .on_drag(|s: &mut State, d, _| {
            let Some(c) = crop(s) else { return };
            if d.phase == Phase::Down {
                c.grab = (c.cx, c.cy);
            }
            let scale = VIEW / c.side();
            c.cx = c.grab.0 - d.dx / scale;
            c.cy = c.grab.1 - d.dy / scale;
            c.clamp();
        })
        .on_wheel(|s: &mut State, e, _| {
            let Some(c) = crop(s) else { return false };
            c.zoom *= (1.0 + e.dy * 0.004).clamp(0.5, 2.0);
            c.clamp();
            true
        });

    let frac = (c.zoom - 1.0) / (MAX_ZOOM - 1.0);
    let zoom = row()
        .w_full()
        .gap(12.0)
        .center_y()
        .child(glyph(&icons::DOWN, 14.0, p.faint))
        .child(slider("crop-zoom", frac, |s, f| {
            if let Some(c) = crop(s) {
                c.zoom = 1.0 + f * (MAX_ZOOM - 1.0);
                c.clamp();
            }
        }))
        .child(glyph(&icons::UP, 14.0, p.faint));

    let mut previews = row().gap(14.0).center_y().child(text("Preview").font(type_::LABEL).color(p.faint));
    for size in [64.0, 40.0, 24.0] {
        previews = previews.child(framed(c, size, true));
    }

    let card = col()
        .key("crop-card")
        .w(VIEW + 48.0)
        .padding(24.0)
        .gap(16.0)
        .radius(14.0)
        .fill(p.panel)
        .border(1.0, p.line)
        .shadow(12.0, 40.0, oklcha(0.0, 0.0, 0.0, if p.dark { 0.45 } else { 0.2 }))
        .transition(rune::Transition::scale(0.97))
        .child(text("Crop your avatar").font(TextStyle::new(17.0, 650.0)).color(p.text))
        .child(text("Drag to move it, scroll or use the slider to zoom.").font(type_::LABEL).color(p.faint))
        .child(viewport)
        .child(zoom)
        .child(previews)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(el().grow(1.0))
                .child(button("crop-cancel", "Cancel", 0).on_press(|s: &mut State| {
                    if let Some(set) = &mut s.settings {
                        set.crop = None;
                    }
                }))
                .child(button("crop-apply", "Apply", 1).on_press(apply)),
        )
        .on_press(|_: &mut State| {});
    el().key("crop-layer")
        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .fill(oklcha(0.0, 0.0, 0.0, if p.dark { 0.45 } else { 0.25 }))
        .center()
        .on_press(|_: &mut State| {})
        .child(card)
}

/// Keep the crop as the new (unsaved) avatar.
fn apply(s: &mut State) {
    let Some(set) = &mut s.settings else { return };
    let Some(c) = set.crop.take() else { return };
    match c.png() {
        Ok(png) => {
            let px = rune::Pixels::new();
            if let Ok(img) = image::load_from_memory(&png) {
                let rgba = img.to_rgba8();
                px.set(rgba.width(), rgba.height(), rgba.into_raw());
            }
            set.avatar = Some((png, px));
        }
        Err(e) => s.toast(format!("Couldn't crop that image: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide() -> Crop {
        // 300×200: red left half, blue right half.
        let rgba = (0..200).flat_map(|_| (0..300).flat_map(|x| if x < 150 { [255, 0, 0, 255] } else { [0, 0, 255, 255] })).collect();
        Crop::new(300, 200, rgba)
    }

    #[test]
    fn starts_centred_and_stays_inside() {
        let mut c = wide();
        assert_eq!(c.rect(), (50, 0, 200));
        c.cx = -1000.0;
        c.clamp();
        assert_eq!(c.rect(), (0, 0, 200), "can't drag past the left edge");
        c.zoom = 2.0;
        c.cx = 1e6;
        c.cy = 1e6;
        c.clamp();
        assert_eq!(c.rect(), (200, 100, 100), "zoomed in, pushed to the bottom-right corner");
        c.zoom = 99.0;
        c.clamp();
        assert_eq!(c.zoom, MAX_ZOOM);
    }

    #[test]
    fn exports_what_the_frame_shows() {
        let mut c = wide();
        c.zoom = 2.0;
        c.cx = 300.0; // all the way right: only blue in the frame
        c.clamp();
        let png = c.png().unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (OUT, OUT));
        assert!(img.pixels().all(|p| p.0[2] > 200 && p.0[0] < 50), "a blue avatar");
    }
}
