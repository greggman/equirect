use bytemuck::{Pod, Zeroable};

/// Axis-aligned rectangle in pixel coordinates.  Top-left origin (y down).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self { Self { x, y, w, h } }
    pub fn right(&self)  -> f32 { self.x + self.w }
    pub fn bottom(&self) -> f32 { self.y + self.h }
    #[allow(dead_code)]
    pub fn center(&self) -> (f32, f32) { (self.x + self.w * 0.5, self.y + self.h * 0.5) }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn shrink(&self, dx: f32, dy: f32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, w: (self.w - dx * 2.0).max(0.0), h: (self.h - dy * 2.0).max(0.0) }
    }
    pub fn intersect(&self, o: Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        if x1 > x0 && y1 > y0 {
            Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
        } else {
            Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }
        }
    }
}

/// Premultiplied RGBA in 0..1.
#[derive(Copy, Clone, Debug, Default)]
pub struct Color { pub r: f32, pub g: f32, pub b: f32, pub a: f32 }

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self { Self { r, g, b, a } }
    #[allow(dead_code)]
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self { Self { r, g, b, a: 1.0 } }
    #[allow(dead_code)]
    pub fn from_u8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: a as f32 / 255.0 }
    }
    #[allow(dead_code)]
    pub fn with_alpha(self, a: f32) -> Self { Self { a, ..self } }
}

#[repr(u32)]
#[derive(Copy, Clone)]
pub enum QuadKind {
    Solid  = 0,
    Glyph  = 1,
    Sprite = 2,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct Vertex {
    pub pos:   [f32; 2],
    pub uv:    [f32; 2],
    pub color: [f32; 4],
    pub mode:  u32,
    pub _pad:  u32,
}

/// One draw call's worth of vertices, all sharing the same scissor (if any).
#[derive(Clone)]
pub struct DrawCmd {
    pub vertex_start: u32,
    pub vertex_count: u32,
    pub clip: Option<[u32; 4]>,
}

/// Accumulator for one frame's worth of UI geometry.  The renderer walks
/// `cmds` once and consumes `vertices` as a single GPU buffer.
pub struct DrawList {
    pub vertices: Vec<Vertex>,
    pub cmds:     Vec<DrawCmd>,
    clip_stack:   Vec<Rect>,      // intersected clip rects, top of stack is active
    /// Vertex index where the currently-open command started.  Closed when
    /// the clip changes or the frame ends.
    open_start:   u32,
    /// Clip used for the currently-open command.
    open_clip:    Option<[u32; 4]>,
}

impl DrawList {
    pub fn new() -> Self {
        Self {
            vertices:   Vec::new(),
            cmds:       Vec::new(),
            clip_stack: Vec::new(),
            open_start: 0,
            open_clip:  None,
        }
    }

    pub fn clear(&mut self) {
        self.vertices.clear();
        self.cmds.clear();
        self.clip_stack.clear();
        self.open_start = 0;
        self.open_clip  = None;
    }

    /// Push a clip rect (intersected with the current one) onto the stack.
    pub fn push_clip(&mut self, rect: Rect) {
        let new = match self.clip_stack.last() {
            Some(top) => top.intersect(rect),
            None => rect,
        };
        self.flush_cmd();
        self.clip_stack.push(new);
        self.open_clip = Some(rect_to_scissor(new));
    }

    pub fn pop_clip(&mut self) {
        if self.clip_stack.is_empty() { return; }
        self.flush_cmd();
        self.clip_stack.pop();
        self.open_clip = self.clip_stack.last().map(|r| rect_to_scissor(*r));
    }

    /// End the frame: emit the final draw cmd if there are open vertices.
    pub fn finish(&mut self) {
        self.flush_cmd();
    }

    // ── primitive emitters ─────────────────────────────────────────────────

    pub fn rect(&mut self, r: Rect, color: Color) {
        if color.a <= 0.0 || r.w <= 0.0 || r.h <= 0.0 { return; }
        self.push_quad(r, [0.0, 0.0, 0.0, 0.0], color, QuadKind::Solid);
    }

    #[allow(dead_code)]
    pub fn rect_outline(&mut self, r: Rect, color: Color, thickness: f32) {
        if color.a <= 0.0 || thickness <= 0.0 { return; }
        // four thin rects: top, bottom, left, right.
        self.rect(Rect::new(r.x, r.y, r.w, thickness), color);
        self.rect(Rect::new(r.x, r.bottom() - thickness, r.w, thickness), color);
        self.rect(Rect::new(r.x, r.y + thickness, thickness, r.h - 2.0 * thickness), color);
        self.rect(Rect::new(r.right() - thickness, r.y + thickness, thickness, r.h - 2.0 * thickness), color);
    }

    pub fn glyph(&mut self, screen: Rect, uv: [f32; 4], color: Color) {
        if color.a <= 0.0 || screen.w <= 0.0 || screen.h <= 0.0 { return; }
        self.push_quad(screen, uv, color, QuadKind::Glyph);
    }

    pub fn sprite(&mut self, screen: Rect, uv: [f32; 4], tint: Color) {
        if tint.a <= 0.0 || screen.w <= 0.0 || screen.h <= 0.0 { return; }
        self.push_quad(screen, uv, tint, QuadKind::Sprite);
    }

    // ── internals ──────────────────────────────────────────────────────────

    fn push_quad(&mut self, r: Rect, uv: [f32; 4], color: Color, kind: QuadKind) {
        let c = [color.r, color.g, color.b, color.a];
        let mode = kind as u32;
        let x0 = r.x; let y0 = r.y;
        let x1 = r.x + r.w; let y1 = r.y + r.h;
        let [u0, v0, u1, v1] = uv;
        // Two triangles, six verts.  Top-left, bottom-left, top-right, top-right, bottom-left, bottom-right.
        self.vertices.push(Vertex { pos: [x0, y0], uv: [u0, v0], color: c, mode, _pad: 0 });
        self.vertices.push(Vertex { pos: [x0, y1], uv: [u0, v1], color: c, mode, _pad: 0 });
        self.vertices.push(Vertex { pos: [x1, y0], uv: [u1, v0], color: c, mode, _pad: 0 });
        self.vertices.push(Vertex { pos: [x1, y0], uv: [u1, v0], color: c, mode, _pad: 0 });
        self.vertices.push(Vertex { pos: [x0, y1], uv: [u0, v1], color: c, mode, _pad: 0 });
        self.vertices.push(Vertex { pos: [x1, y1], uv: [u1, v1], color: c, mode, _pad: 0 });
    }

    fn flush_cmd(&mut self) {
        let end = self.vertices.len() as u32;
        if end > self.open_start {
            self.cmds.push(DrawCmd {
                vertex_start: self.open_start,
                vertex_count: end - self.open_start,
                clip:         self.open_clip,
            });
        }
        self.open_start = end;
    }
}

fn rect_to_scissor(r: Rect) -> [u32; 4] {
    let x = r.x.max(0.0) as u32;
    let y = r.y.max(0.0) as u32;
    let w = r.w.max(0.0) as u32;
    let h = r.h.max(0.0) as u32;
    [x, y, w, h]
}
