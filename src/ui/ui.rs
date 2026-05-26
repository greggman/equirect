//! Immediate-mode UI core.  Owns the layout cursor, the cursor-input model,
//! per-widget persistent state, and emits geometry into a `DrawList`.
//!
//! Two cursors are supported natively: every widget hit-tests both, so two
//! hands can hover (and click) different buttons in the same frame.

use std::collections::HashMap;

use super::atlas::GlyphAtlas;
use super::draw_list::{Color, DrawList, Rect};
use super::icons::{ICONS_SHEET_HEIGHT, ICONS_SHEET_WIDTH, IconSprite};

// ── style constants (tweak globally) ──────────────────────────────────────────

pub const PANEL_BG:        Color = Color { r: 15.0/255.0, g: 15.0/255.0, b: 15.0/255.0, a: 220.0/255.0 };
pub const TEXT:            Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
pub const TEXT_MUTED:      Color = Color { r: 0.65, g: 0.65, b: 0.65, a: 1.0 };
pub const TEXT_DISABLED:   Color = Color { r: 0.40, g: 0.40, b: 0.40, a: 1.0 };
#[allow(dead_code)]
pub const ACCENT:          Color = Color { r: 0.40, g: 0.60, b: 0.95, a: 1.0 };
pub const BTN_BG:          Color = Color { r: 0.18, g: 0.18, b: 0.18, a: 1.0 };
pub const BTN_BG_HOVER:    Color = Color { r: 0.28, g: 0.28, b: 0.28, a: 1.0 };
pub const BTN_BG_ACTIVE:   Color = Color { r: 0.10, g: 0.10, b: 0.10, a: 1.0 };
pub const BTN_BG_SELECTED: Color = Color { r: 0.28, g: 0.42, b: 0.66, a: 1.0 };
pub const SEPARATOR:       Color = Color { r: 0.30, g: 0.30, b: 0.30, a: 1.0 };
pub const SLIDER_TRACK:    Color = Color { r: 0.22, g: 0.22, b: 0.22, a: 1.0 };
pub const SLIDER_FILL:     Color = Color { r: 0.40, g: 0.60, b: 0.95, a: 1.0 };
pub const SLIDER_THUMB:    Color = Color { r: 0.85, g: 0.85, b: 0.85, a: 1.0 };
pub const ROW_HOVER:       Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.10 };
pub const ROW_SELECTED:    Color = Color { r: 0.16, g: 0.24, b: 0.36, a: 1.0 };

pub const PADDING:         f32 = 12.0;
pub const ITEM_SPACING_Y:  f32 = 6.0;
pub const ITEM_SPACING_X:  f32 = 6.0;
pub const FONT_DEFAULT:    f32 = 22.0;
pub const FONT_SMALL:      f32 = 16.0;
pub const FONT_TINY:       f32 = 12.0;

// ── Cursor input ──────────────────────────────────────────────────────────────

#[derive(Copy, Clone, Default)]
pub struct CursorInput {
    /// Pixel position on the panel.  `None` if the cursor doesn't hit it.
    pub pos:      Option<(f32, f32)>,
    pub clicking: bool,
    /// Vertical scroll input (pixels per frame).  Positive scrolls down.
    /// Drives `scroll_area` when any cursor hovers it.
    pub scroll_y: f32,
}

/// Press / release pair for one cursor on the frame the button was released.
type Interaction = ((f32, f32), (f32, f32));

// ── Persistent state (lives across frames; owned by the panel) ───────────────

#[derive(Default)]
pub struct UiState {
    prev_clicking: [bool; 2],
    press_pos:     [Option<(f32, f32)>; 2],
    /// Which cursor (0 or 1) currently owns a drag, keyed by widget ID.
    drag:          HashMap<u64, u8>,
    /// Saved scroll offsets, keyed by scroll-area ID.
    scroll_y:      HashMap<u64, f32>,
}

impl UiState {
    pub fn new() -> Self { Self::default() }
}

// ── Layout ────────────────────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq)]
enum Direction { Vertical, HorizontalLr, HorizontalRl }

#[derive(Copy, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Align { Left, Center, Right }

#[derive(Copy, Clone)]
struct LayoutCtx {
    /// Bounds available for this layout (absolute panel coords).
    rect:      Rect,
    /// Origin x of the current row (re-set when wrapping; we don't actually wrap).
    #[allow(dead_code)]
    initial_x: f32,
    /// Next widget anchor.  In vertical layouts this is (rect.x, current_y).
    /// In horizontal LR it's (current_x, rect.y); RTL flips to right-anchored.
    cursor_x:  f32,
    cursor_y:  f32,
    direction: Direction,
    /// Tallest widget so far in the current row (only meaningful for horizontal).
    row_height: f32,
}

// ── Ui ────────────────────────────────────────────────────────────────────────

pub struct Ui<'a> {
    pub draw:    &'a mut DrawList,
    atlas:       &'a mut GlyphAtlas,

    state:       &'a mut UiState,

    layouts:     Vec<LayoutCtx>,
    next_id:     u64,

    cursors:     [CursorInput; 2],
    interactions:[Option<Interaction>; 2],

    enabled:     bool,
}

/// Begin one UI frame.  Compute press→release transitions, set up the root
/// vertical layout filling the panel.  The returned `Ui` borrows everything
/// for the lifetime of the frame.
pub fn begin_frame<'a>(
    state:      &'a mut UiState,
    draw:       &'a mut DrawList,
    atlas:      &'a mut GlyphAtlas,
    panel_size: (f32, f32),
    cursors:    [CursorInput; 2],
) -> Ui<'a> {
    // Update click tracking and emit (press, release) pairs on the release frame.
    let mut interactions: [Option<Interaction>; 2] = [None, None];
    for i in 0..2 {
        let now  = cursors[i].clicking;
        let prev = state.prev_clicking[i];
        let pos  = cursors[i].pos;

        if now && !prev {
            state.press_pos[i] = pos;
        }
        if !now && prev {
            if let (Some(p), Some(r)) = (state.press_pos[i], pos) {
                interactions[i] = Some((p, r));
            }
            state.press_pos[i] = None;
        }
        state.prev_clicking[i] = now;
    }

    let root_rect = Rect::new(0.0, 0.0, panel_size.0, panel_size.1);
    let inner = root_rect.shrink(PADDING, PADDING);

    let layout = LayoutCtx {
        rect:       inner,
        initial_x:  inner.x,
        cursor_x:   inner.x,
        cursor_y:   inner.y,
        direction:  Direction::Vertical,
        row_height: 0.0,
    };

    Ui {
        draw,
        atlas,
        state,
        layouts: vec![layout],
        next_id: 1,
        cursors,
        interactions,
        enabled: true,
    }
}

impl Ui<'_> {
    /// Paint the panel background (skip if you composite onto something else).
    pub fn paint_background(&mut self, color: Color) {
        let r = self.layouts[0].rect;
        let full = Rect::new(r.x - PADDING, r.y - PADDING, r.w + 2.0 * PADDING, r.h + 2.0 * PADDING);
        // Insert at z=back: simplest is to push it before the user's content,
        // but draw_list is a single linear stream — caller should call this first.
        self.draw.rect(full, color);
    }

    // ── layout primitives ─────────────────────────────────────────────────────

    fn current(&self) -> &LayoutCtx { self.layouts.last().expect("layout stack empty") }
    fn current_mut(&mut self) -> &mut LayoutCtx { self.layouts.last_mut().expect("layout stack empty") }

    /// Reserve a (w × h) rect from the active layout.
    pub fn allocate(&mut self, w: f32, h: f32) -> Rect {
        let l = *self.current();
        let rect = match l.direction {
            Direction::Vertical => {
                Rect::new(l.cursor_x, l.cursor_y, w.min(l.rect.w), h)
            }
            Direction::HorizontalLr => {
                Rect::new(l.cursor_x, l.cursor_y, w, h)
            }
            Direction::HorizontalRl => {
                Rect::new(l.cursor_x - w, l.cursor_y, w, h)
            }
        };
        let m = self.current_mut();
        match m.direction {
            Direction::Vertical => {
                m.cursor_y += h + ITEM_SPACING_Y;
            }
            Direction::HorizontalLr => {
                m.cursor_x += w + ITEM_SPACING_X;
                m.row_height = m.row_height.max(h);
            }
            Direction::HorizontalRl => {
                m.cursor_x -= w + ITEM_SPACING_X;
                m.row_height = m.row_height.max(h);
            }
        }
        rect
    }

    /// Reserve full available width × given height.  Convenience for vertical
    /// layouts; in horizontal layouts it just uses h as-is and width = remaining.
    pub fn allocate_full_width(&mut self, h: f32) -> Rect {
        let w = self.available_width();
        self.allocate(w, h)
    }

    /// Width remaining in the active layout for the next widget.
    pub fn available_width(&self) -> f32 {
        let l = self.current();
        match l.direction {
            Direction::Vertical      => l.rect.w,
            Direction::HorizontalLr  => (l.rect.right() - l.cursor_x).max(0.0),
            Direction::HorizontalRl  => (l.cursor_x - l.rect.x).max(0.0),
        }
    }

    pub fn available_height(&self) -> f32 {
        let l = self.current();
        (l.rect.bottom() - l.cursor_y).max(0.0)
    }

    pub fn add_space(&mut self, amount: f32) {
        let m = self.current_mut();
        match m.direction {
            Direction::Vertical      => m.cursor_y += amount,
            Direction::HorizontalLr  => m.cursor_x += amount,
            Direction::HorizontalRl  => m.cursor_x -= amount,
        }
    }

    /// Run `content` inside a horizontal LTR row.  When the closure returns,
    /// the parent layout advances by the row's tallest widget height.
    pub fn horizontal<R>(&mut self, content: impl FnOnce(&mut Ui) -> R) -> R {
        self.row(Direction::HorizontalLr, content)
    }

    /// Same as `horizontal`, but widgets are placed from the right edge inward.
    pub fn horizontal_rtl<R>(&mut self, content: impl FnOnce(&mut Ui) -> R) -> R {
        self.row(Direction::HorizontalRl, content)
    }

    fn row<R>(&mut self, dir: Direction, content: impl FnOnce(&mut Ui) -> R) -> R {
        let parent = *self.current();
        // Compute the row bounds based on the parent's direction so nested
        // rows can split the parent's remaining horizontal space.
        let (row_left, row_top, row_w) = match parent.direction {
            Direction::Vertical      => (parent.rect.x, parent.cursor_y, parent.rect.w),
            Direction::HorizontalLr  => (parent.cursor_x, parent.cursor_y, (parent.rect.right() - parent.cursor_x).max(0.0)),
            Direction::HorizontalRl  => (parent.rect.x,  parent.cursor_y, (parent.cursor_x - parent.rect.x).max(0.0)),
        };
        let row_h = (parent.rect.bottom() - row_top).max(0.0);

        let layout = LayoutCtx {
            rect:       Rect::new(row_left, row_top, row_w, row_h),
            initial_x:  row_left,
            cursor_x:   if dir == Direction::HorizontalRl { row_left + row_w } else { row_left },
            cursor_y:   row_top,
            direction:  dir,
            row_height: 0.0,
        };
        self.layouts.push(layout);
        let result = content(self);
        let used = self.current().row_height;
        self.layouts.pop();

        let m = self.current_mut();
        match m.direction {
            Direction::Vertical => {
                m.cursor_y = row_top + used + ITEM_SPACING_Y;
            }
            Direction::HorizontalLr => {
                m.cursor_x = row_left + row_w;
                m.row_height = m.row_height.max(used);
            }
            Direction::HorizontalRl => {
                m.cursor_x = row_left;
                m.row_height = m.row_height.max(used);
            }
        }
        result
    }

    /// Run `content` with `enabled = false`: widgets render dimmed and don't
    /// react to clicks.  Restores the previous flag on return.
    pub fn disabled<R>(&mut self, disabled: bool, content: impl FnOnce(&mut Ui) -> R) -> R {
        let prev = self.enabled;
        self.enabled = prev && !disabled;
        let r = content(self);
        self.enabled = prev;
        r
    }

    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool { self.enabled }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    // ── input queries ─────────────────────────────────────────────────────────

    /// True if any cursor is currently inside `rect`.
    pub fn hovered(&self, rect: Rect) -> bool {
        if !self.enabled { return false; }
        self.cursors.iter().any(|c| match c.pos {
            Some((x, y)) => rect.contains(x, y),
            None => false,
        })
    }

    /// True if any cursor is currently held down inside `rect` (for "active" visuals).
    pub fn pressed(&self, rect: Rect) -> bool {
        if !self.enabled { return false; }
        for i in 0..2 {
            if self.cursors[i].clicking {
                if let Some((x, y)) = self.cursors[i].pos {
                    if rect.contains(x, y) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// True on the single frame any cursor's press+release pair both fall in `rect`.
    pub fn activated(&self, rect: Rect) -> bool {
        if !self.enabled { return false; }
        self.interactions.iter().any(|it| match it {
            Some((p, r)) => rect.contains(p.0, p.1) && rect.contains(r.0, r.1),
            None => false,
        })
    }

    // ── widgets ───────────────────────────────────────────────────────────────

    /// Plain text label, default font and color.
    #[allow(dead_code)]
    pub fn label(&mut self, text: &str) {
        self.label_styled(text, FONT_DEFAULT, if self.enabled { TEXT } else { TEXT_DISABLED });
    }

    pub fn label_styled(&mut self, text: &str, font_size: f32, color: Color) {
        let (glyphs, width) = self.atlas.shape(text, font_size);
        let line_h          = self.atlas.line_height(font_size).ceil();
        let ascent          = self.atlas.ascent(font_size);

        let rect = self.allocate(width.ceil().min(self.available_width()), line_h);
        let baseline_y = rect.y + ascent;
        let pen_x      = rect.x;

        for g in glyphs {
            let info = g.info;
            if info.width <= 0.0 || info.height <= 0.0 { continue; }
            let gx = pen_x + g.x + info.bearing_x;
            let gy = baseline_y - info.bearing_y;
            self.draw.glyph(
                Rect::new(gx, gy, info.width, info.height),
                [info.u0, info.v0, info.u1, info.v1],
                color,
            );
        }
    }

    /// A text button.  Returns true on the frame any cursor's press+release lands inside.
    pub fn button(&mut self, text: &str) -> bool {
        self.button_styled(text, FONT_DEFAULT)
    }

    pub fn button_styled(&mut self, text: &str, font_size: f32) -> bool {
        let text_w = self.atlas.measure(text, font_size).ceil();
        let pad_x  = 12.0;
        let pad_y  = 6.0;
        let line_h = self.atlas.line_height(font_size).ceil();
        let h      = line_h + pad_y * 2.0;
        let w      = text_w + pad_x * 2.0;

        let rect = self.allocate(w, h);
        self.paint_button(rect, false);
        // Centre the text inside the rect.
        let ascent  = self.atlas.ascent(font_size);
        let base_y  = rect.y + (rect.h - line_h) * 0.5 + ascent;
        let start_x = rect.x + (rect.w - text_w) * 0.5;
        let color   = if self.enabled { TEXT } else { TEXT_DISABLED };
        let (glyphs, _) = self.atlas.shape(text, font_size);
        for g in glyphs {
            let info = g.info;
            if info.width <= 0.0 || info.height <= 0.0 { continue; }
            self.draw.glyph(
                Rect::new(start_x + g.x + info.bearing_x, base_y - info.bearing_y, info.width, info.height),
                [info.u0, info.v0, info.u1, info.v1],
                color,
            );
        }
        self.activated(rect)
    }

    /// Icon button using a sprite from the icon sheet.  `selected` paints a
    /// highlighted background.  `size` is the icon's draw size in pixels.
    pub fn icon_button(&mut self, sprite: IconSprite, size: f32, selected: bool) -> bool {
        let pad_x = 8.0;
        let pad_y = 6.0;
        let rect  = self.allocate(size + pad_x * 2.0, size + pad_y * 2.0);

        if selected {
            self.draw.rect(rect, BTN_BG_SELECTED);
        } else {
            self.paint_button(rect, false);
        }

        let icon_rect = Rect::new(
            rect.x + (rect.w - size) * 0.5,
            rect.y + (rect.h - size) * 0.5,
            size, size,
        );
        let uv = sprite_uv(sprite);
        let tint = if self.enabled { Color::rgba(1.0, 1.0, 1.0, 1.0) } else { Color::rgba(1.0, 1.0, 1.0, 0.4) };
        self.draw.sprite(icon_rect, uv, tint);

        self.activated(rect)
    }

    /// Single-pixel divider line spanning the available width.
    pub fn separator(&mut self) {
        let rect = self.allocate(self.available_width(), 1.0);
        self.draw.rect(rect, SEPARATOR);
        // a bit of breathing room after
        self.add_space(2.0);
    }

    /// Width of `text` in pixels at `font_size` — caches glyphs as a side effect.
    pub fn measure_text(&mut self, text: &str, font_size: f32) -> f32 {
        self.atlas.measure(text, font_size).ceil()
    }

    /// Pre-seed the saved scroll offset of a scroll area before it renders.
    /// Used by the browser to bring the current video into view after a
    /// navigation.
    pub fn set_scroll_offset(&mut self, id: u64, offset: f32) {
        self.state.scroll_y.insert(id, offset.max(0.0));
    }

    /// Render `text` inside `rect` at the given size and color.  Vertical
    /// alignment is centered; horizontal alignment is `align`.
    pub fn draw_text_in(&mut self, rect: Rect, text: &str, font_size: f32, color: Color, align: Align) {
        let (glyphs, width) = self.atlas.shape(text, font_size);
        let line_h          = self.atlas.line_height(font_size).ceil();
        let ascent          = self.atlas.ascent(font_size);
        let baseline_y      = rect.y + (rect.h - line_h) * 0.5 + ascent;
        let pen_x = match align {
            Align::Left   => rect.x + 8.0,
            Align::Center => rect.x + (rect.w - width).max(0.0) * 0.5,
            Align::Right  => rect.right() - 8.0 - width,
        };
        for g in glyphs {
            let info = g.info;
            if info.width <= 0.0 || info.height <= 0.0 { continue; }
            self.draw.glyph(
                Rect::new(pen_x + g.x + info.bearing_x, baseline_y - info.bearing_y, info.width, info.height),
                [info.u0, info.v0, info.u1, info.v1],
                color,
            );
        }
    }

    /// Horizontal slider over `value` clamped to [min, max].  Continuous drag:
    /// while a cursor is held inside the track, value follows the cursor.
    /// Returns true on any frame the value changes.
    #[allow(dead_code)]
    pub fn slider(&mut self, value: &mut f32, min: f32, max: f32) -> bool {
        let w = self.available_width();
        self.slider_sized(w, value, min, max)
    }

    pub fn slider_sized(&mut self, width: f32, value: &mut f32, min: f32, max: f32) -> bool {
        let id     = self.next_id();
        let h      = 22.0;
        let rect   = self.allocate(width, h);
        let track  = rect.shrink(0.0, 8.0);

        let mut changed = false;

        // Drag handling
        let dragging_cursor = self.state.drag.get(&id).copied();
        let active_cursor   = if let Some(c) = dragging_cursor {
            if self.cursors[c as usize].clicking {
                Some(c as usize)
            } else {
                self.state.drag.remove(&id);
                None
            }
        } else {
            // Look for a press that started on this frame inside the track.
            let mut found = None;
            for i in 0..2 {
                if self.cursors[i].clicking {
                    if let Some(p) = self.state.press_pos[i] {
                        if track.contains(p.0, p.1) && self.enabled {
                            found = Some(i);
                            break;
                        }
                    }
                }
            }
            if let Some(i) = found {
                self.state.drag.insert(id, i as u8);
            }
            found
        };

        if let Some(i) = active_cursor {
            if let Some((cx, _cy)) = self.cursors[i].pos {
                let frac     = ((cx - track.x) / track.w.max(1.0)).clamp(0.0, 1.0);
                let new_val  = min + frac * (max - min);
                if (new_val - *value).abs() > 1e-6 {
                    *value  = new_val;
                    changed = true;
                }
            }
        }

        // Draw track + fill + thumb.
        let frac = ((*value - min) / (max - min).max(1e-6)).clamp(0.0, 1.0);
        self.draw.rect(track, SLIDER_TRACK);
        let fill_w = track.w * frac;
        self.draw.rect(Rect::new(track.x, track.y, fill_w, track.h), SLIDER_FILL);
        let thumb_w = 10.0;
        let thumb_x = track.x + track.w * frac - thumb_w * 0.5;
        let thumb_y = rect.y + 2.0;
        self.draw.rect(Rect::new(thumb_x, thumb_y, thumb_w, rect.h - 4.0), SLIDER_THUMB);

        changed
    }

    /// Scroll area: fixed-height viewport over arbitrary-height content.  The
    /// caller draws content inside `content`, which receives a Ui scoped to a
    /// scrollable inner rect.  Use `id` to keep the scroll position stable
    /// across frames.
    pub fn scroll_area<R>(&mut self, id: u64, height: f32, content: impl FnOnce(&mut Ui) -> R) -> R {
        let viewport = self.allocate(self.available_width(), height);
        let scrollbar_w = 12.0;
        let inner_rect  = Rect::new(viewport.x, viewport.y, viewport.w - scrollbar_w - 4.0, viewport.h);

        let mut offset = self.state.scroll_y.get(&id).copied().unwrap_or(0.0);

        // Apply any pending scroll input from cursors hovering the viewport.
        for c in &self.cursors {
            if c.scroll_y != 0.0 {
                if let Some((x, y)) = c.pos {
                    if viewport.contains(x, y) {
                        offset += c.scroll_y;
                    }
                }
            }
        }

        // Push a clipped vertical layout shifted by -offset.
        self.draw.push_clip(inner_rect);
        let layout = LayoutCtx {
            rect:      Rect::new(inner_rect.x, inner_rect.y - offset, inner_rect.w, f32::INFINITY),
            initial_x: inner_rect.x,
            cursor_x:  inner_rect.x,
            cursor_y:  inner_rect.y - offset,
            direction: Direction::Vertical,
            row_height: 0.0,
        };
        self.layouts.push(layout);
        let result = content(self);
        let bottom_y = self.current().cursor_y;
        self.layouts.pop();
        self.draw.pop_clip();

        let content_h = (bottom_y - (inner_rect.y - offset)).max(0.0);

        // Clamp offset.
        let max_off = (content_h - viewport.h).max(0.0);
        if offset > max_off { offset = max_off; }
        if offset < 0.0 { offset = 0.0; }

        // Scrollbar drag.
        let bar_rect    = Rect::new(viewport.right() - scrollbar_w, viewport.y, scrollbar_w, viewport.h);
        self.draw.rect(bar_rect, Color::rgba(0.10, 0.10, 0.10, 0.6));
        if content_h > viewport.h {
            let thumb_h = (viewport.h * viewport.h / content_h).max(20.0);
            let thumb_y = bar_rect.y + (bar_rect.h - thumb_h) * (offset / max_off.max(1.0));
            let thumb_rect = Rect::new(bar_rect.x + 2.0, thumb_y, bar_rect.w - 4.0, thumb_h);
            self.draw.rect(thumb_rect, Color::rgba(0.55, 0.55, 0.55, 0.9));

            // Drag the thumb.
            let drag_id = id ^ 0xDEADBEEF;
            let dragging_cursor = self.state.drag.get(&drag_id).copied();
            let active = if let Some(c) = dragging_cursor {
                if self.cursors[c as usize].clicking {
                    Some(c as usize)
                } else {
                    self.state.drag.remove(&drag_id);
                    None
                }
            } else {
                let mut found = None;
                for i in 0..2 {
                    if self.cursors[i].clicking {
                        if let Some(p) = self.state.press_pos[i] {
                            if bar_rect.contains(p.0, p.1) {
                                found = Some(i);
                                break;
                            }
                        }
                    }
                }
                if let Some(i) = found {
                    self.state.drag.insert(drag_id, i as u8);
                }
                found
            };
            if let Some(i) = active {
                if let Some((_x, y)) = self.cursors[i].pos {
                    let frac = ((y - bar_rect.y - thumb_h * 0.5) / (bar_rect.h - thumb_h).max(1.0)).clamp(0.0, 1.0);
                    offset = frac * max_off;
                }
            }
        }

        self.state.scroll_y.insert(id, offset);
        result
    }

    // ── internals ─────────────────────────────────────────────────────────────

    fn paint_button(&mut self, rect: Rect, force_active: bool) {
        let bg = if !self.enabled {
            BTN_BG
        } else if force_active || self.pressed(rect) {
            BTN_BG_ACTIVE
        } else if self.hovered(rect) {
            BTN_BG_HOVER
        } else {
            BTN_BG
        };
        self.draw.rect(rect, bg);
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn sprite_uv(sprite: IconSprite) -> [f32; 4] {
    [
        sprite.x / ICONS_SHEET_WIDTH,
        sprite.y / ICONS_SHEET_HEIGHT,
        (sprite.x + sprite.w) / ICONS_SHEET_WIDTH,
        (sprite.y + sprite.h) / ICONS_SHEET_HEIGHT,
    ]
}
