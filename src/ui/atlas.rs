use std::collections::HashMap;

use ab_glyph::{Font, FontVec, ScaleFont};

/// Glyph atlas: a single R8Unorm texture into which we rasterise glyphs on
/// demand using `ab_glyph`.  Lookups are cached by (font, glyph, size).
///
/// Multiple fonts are supported for fallback: when shaping text, the primary
/// font is checked first, then each fallback in order.  This is how we get
/// CJK glyphs from system fonts without bundling them.
///
/// Packing is a simple shelf algorithm — we never evict, but the atlas is
/// large enough (1024×1024) that the working set for our few fixed sizes
/// fits comfortably.
pub struct GlyphAtlas {
    fonts: Vec<FontVec>,

    width:    u32,
    height:   u32,
    pixels:   Vec<u8>,                // CPU mirror; reuploaded after additions
    texture:  wgpu::Texture,
    view:     wgpu::TextureView,
    dirty:    bool,                   // true while pixels has unsynced changes

    cache:      HashMap<GlyphKey, GlyphInfo>,
    /// Maps a (codepoint, size) to (font_idx, glyph_id) chosen by fallback.
    /// Avoids walking the fallback chain every frame for the same character.
    shape_cache: HashMap<ShapeKey, ShapeEntry>,

    next_x:     u32,
    next_y:     u32,
    row_height: u32,
}

#[derive(Hash, PartialEq, Eq, Copy, Clone)]
struct GlyphKey {
    font_idx: u16,
    glyph_id: u16,
    size_px:  u16,
}

#[derive(Hash, PartialEq, Eq, Copy, Clone)]
struct ShapeKey {
    codepoint: u32,
    size_px:   u16,
}

#[derive(Copy, Clone)]
struct ShapeEntry {
    font_idx: u16,
    glyph_id: u16,
}

/// Information needed to draw one glyph: where in the atlas it lives, its
/// pixel size, and its placement relative to the pen position.
#[derive(Copy, Clone, Debug)]
pub struct GlyphInfo {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub width:     f32,
    pub height:    f32,
    /// Distance from pen position to the left edge of the glyph image (px).
    pub bearing_x: f32,
    /// Distance from baseline to the top of the glyph image (px, positive up).
    pub bearing_y: f32,
    /// Pen advance after this glyph (px).
    pub advance:   f32,
}

/// One placed glyph in a shaped run.
#[derive(Copy, Clone)]
pub struct ShapedGlyph {
    pub info: GlyphInfo,
    /// Pen X at the start of this glyph (relative to the run origin).
    pub x:    f32,
}

impl GlyphAtlas {
    /// `primary` is the font tried first; `fallbacks` are tried in order for
    /// codepoints the primary doesn't have a glyph for.
    pub fn new(
        device:    &wgpu::Device,
        primary:   Vec<u8>,
        fallbacks: Vec<Vec<u8>>,
    ) -> Self {
        let mut fonts = Vec::with_capacity(1 + fallbacks.len());
        fonts.push(FontVec::try_from_vec(primary).expect("primary font failed to parse"));
        for f in fallbacks {
            if let Ok(font) = FontVec::try_from_vec(f) {
                fonts.push(font);
            }
        }

        let width  = 1024;
        let height = 1024;
        let pixels = vec![0u8; (width * height) as usize];

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui_glyph_atlas"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count:    1,
            dimension:       wgpu::TextureDimension::D2,
            format:          wgpu::TextureFormat::R8Unorm,
            usage:           wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats:    &[],
        });
        let view = texture.create_view(&Default::default());

        Self {
            fonts,
            width, height, pixels, texture, view,
            dirty: false,
            cache: HashMap::new(),
            shape_cache: HashMap::new(),
            next_x: 1, next_y: 1, row_height: 0,
        }
    }

    pub fn view(&self) -> &wgpu::TextureView { &self.view }

    /// Find which font has a glyph for `c` at this size, then make sure that
    /// glyph is rasterised into the atlas, and return its placement info.
    fn ensure_glyph(&mut self, c: char, size_px: u16) -> Option<GlyphInfo> {
        let codepoint = c as u32;
        let entry = if let Some(e) = self.shape_cache.get(&ShapeKey { codepoint, size_px }) {
            *e
        } else {
            let mut chosen: Option<ShapeEntry> = None;
            for (i, font) in self.fonts.iter().enumerate() {
                let gid = font.glyph_id(c);
                if gid.0 != 0 || i == self.fonts.len() - 1 {
                    chosen = Some(ShapeEntry { font_idx: i as u16, glyph_id: gid.0 });
                    if gid.0 != 0 { break; }
                }
            }
            let chosen = chosen?;
            self.shape_cache.insert(ShapeKey { codepoint, size_px }, chosen);
            chosen
        };

        let key = GlyphKey { font_idx: entry.font_idx, glyph_id: entry.glyph_id, size_px };
        if let Some(info) = self.cache.get(&key) { return Some(*info); }

        let font   = &self.fonts[entry.font_idx as usize];
        let scaled = font.as_scaled(size_px as f32);
        let glyph  = ab_glyph::Glyph {
            id:       ab_glyph::GlyphId(entry.glyph_id),
            scale:    ab_glyph::PxScale::from(size_px as f32),
            position: ab_glyph::point(0.0, 0.0),
        };
        let advance = scaled.h_advance(glyph.id);

        let outline = font.outline_glyph(glyph);
        let info = match outline {
            None => {
                // Whitespace / non-printing — record only the advance so the
                // shaper can still position later glyphs correctly.
                GlyphInfo {
                    u0: 0.0, v0: 0.0, u1: 0.0, v1: 0.0,
                    width: 0.0, height: 0.0,
                    bearing_x: 0.0, bearing_y: 0.0,
                    advance,
                }
            }
            Some(outline) => {
                let bb = outline.px_bounds();
                let w  = bb.width().ceil() as u32;
                let h  = bb.height().ceil() as u32;

                // Find shelf space (1px gutter).
                if self.next_x + w + 1 > self.width {
                    self.next_x = 1;
                    self.next_y += self.row_height + 1;
                    self.row_height = 0;
                }
                if self.next_y + h + 1 > self.height {
                    // Atlas full — return advance-only metrics so we don't
                    // crash; users will see boxes or missing glyphs.
                    return Some(GlyphInfo {
                        u0: 0.0, v0: 0.0, u1: 0.0, v1: 0.0,
                        width: 0.0, height: 0.0,
                        bearing_x: 0.0, bearing_y: 0.0,
                        advance,
                    });
                }

                let ax = self.next_x;
                let ay = self.next_y;
                let aw = self.width;

                outline.draw(|x, y, c| {
                    let alpha = (c * 255.0).clamp(0.0, 255.0) as u8;
                    let idx = ((ay + y) * aw + (ax + x)) as usize;
                    if idx < self.pixels.len() {
                        self.pixels[idx] = alpha;
                    }
                });
                self.dirty = true;

                self.next_x      += w + 1;
                self.row_height   = self.row_height.max(h);

                GlyphInfo {
                    u0: ax as f32 / self.width as f32,
                    v0: ay as f32 / self.height as f32,
                    u1: (ax + w) as f32 / self.width as f32,
                    v1: (ay + h) as f32 / self.height as f32,
                    width:  w as f32,
                    height: h as f32,
                    bearing_x: bb.min.x,
                    bearing_y: -bb.min.y,
                    advance,
                }
            }
        };

        self.cache.insert(key, info);
        Some(info)
    }

    /// Layout a string at `size_px` along a horizontal pen at y = baseline.
    /// Returns one entry per character along with the total advance width.
    pub fn shape(&mut self, text: &str, size_px: f32) -> (Vec<ShapedGlyph>, f32) {
        let size = size_px.max(1.0) as u16;
        let mut out  = Vec::with_capacity(text.len());
        let mut pen_x = 0.0_f32;
        for c in text.chars() {
            if let Some(info) = self.ensure_glyph(c, size) {
                out.push(ShapedGlyph { info, x: pen_x });
                pen_x += info.advance;
            }
        }
        (out, pen_x)
    }

    /// Width of `text` at the given pixel size, without producing a draw list.
    pub fn measure(&mut self, text: &str, size_px: f32) -> f32 {
        let (_, w) = self.shape(text, size_px);
        w
    }

    /// Distance from baseline to the top of the line, used as the line's
    /// vertical "ascent" when positioning text inside a row.
    pub fn ascent(&self, size_px: f32) -> f32 {
        self.fonts[0].as_scaled(size_px).ascent()
    }

    /// Total line height (ascent + descent + line gap) for the primary font.
    pub fn line_height(&self, size_px: f32) -> f32 {
        let s = self.fonts[0].as_scaled(size_px);
        s.height() + s.line_gap()
    }

    /// Upload any pending glyph rasterisations to the GPU.  Cheap when nothing
    /// has changed since the last call.
    pub fn flush(&mut self, queue: &wgpu::Queue) {
        if !self.dirty { return; }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.pixels,
            wgpu::TexelCopyBufferLayout {
                offset:        0,
                bytes_per_row: Some(self.width),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
        self.dirty = false;
    }
}
