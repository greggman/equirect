// UI shader: draws colored quads, glyph quads (R8 alpha mask) and sprite
// quads (RGBA) into a Rgba8Unorm offscreen texture.  Output is premultiplied
// alpha so the panel-quad pass composites correctly over the video.

struct Viewport { size: vec2f, _pad: vec2f }
@group(0) @binding(0) var<uniform> viewport: Viewport;
@group(0) @binding(1) var glyph_atlas:  texture_2d<f32>;
@group(0) @binding(2) var sprite_atlas: texture_2d<f32>;
@group(0) @binding(3) var atlas_sampler: sampler;

struct VertexIn {
    @location(0) pos:   vec2f,
    @location(1) uv:    vec2f,
    @location(2) color: vec4f,
    @location(3) mode:  u32,
}

struct VertexOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) uv:    vec2f,
    @location(1) color: vec4f,
    @location(2) @interpolate(flat) mode: u32,
}

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    let x =       in.pos.x / viewport.size.x * 2.0 - 1.0;
    let y = 1.0 - in.pos.y / viewport.size.y * 2.0;
    var out: VertexOut;
    out.clip_pos = vec4f(x, y, 0.0, 1.0);
    out.uv       = in.uv;
    out.color    = in.color;
    out.mode     = in.mode;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4f {
    var rgba: vec4f;
    if (in.mode == 0u) {
        rgba = in.color;
    } else if (in.mode == 1u) {
        // Glyph: R8 alpha mask, modulated by tint.
        let alpha = textureSample(glyph_atlas, atlas_sampler, in.uv).r;
        rgba = vec4f(in.color.rgb, in.color.a * alpha);
    } else {
        // Sprite: RGBA texture, modulated by tint.
        let s = textureSample(sprite_atlas, atlas_sampler, in.uv);
        rgba = s * in.color;
    }
    // Premultiply for the (One, OneMinusSrcAlpha) blend at the consumer.
    return vec4f(rgba.rgb * rgba.a, rgba.a);
}
