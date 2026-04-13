// Logo blit shader.
// Draws a full-screen quad (oversized-triangle technique) and samples the logo
// texture with letterbox / pillarbox correction supplied via `LogoParams`.

struct LogoParams {
    // Fraction of NDC space the logo occupies on each axis.
    // scale.x = min(1, (window_h / window_w) * logo_aspect)
    // scale.y = min(1, (window_w / window_h) / logo_aspect)
    scale: vec2<f32>,
    _pad:  vec2<f32>,
}

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0)       ndc:      vec2<f32>,
}

@group(0) @binding(0) var<uniform> params: LogoParams;
@group(1) @binding(0) var logo_tex:     texture_2d<f32>;
@group(1) @binding(1) var logo_sampler: sampler;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOut {
    // Oversized triangle that covers the entire NDC square [-1,1]×[-1,1].
    var x = -1.0;
    var y = -1.0;
    if vi == 1u { x =  3.0; }
    if vi == 2u { y =  3.0; }
    var out: VertexOut;
    out.clip_pos = vec4<f32>(x, y, 0.0, 1.0);
    out.ndc      = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // Map NDC → logo-local coordinates.
    // When scale < 1 on an axis the logo doesn't fill that axis, so points
    // outside [-scale, scale] are in the black border region.
    let logo_xy = in.ndc / params.scale;

    if abs(logo_xy.x) > 1.0 || abs(logo_xy.y) > 1.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0); // black bar
    }

    // logo_xy in [-1,1] → UV in [0,1], Y flipped (NDC +Y up, UV +V down).
    let uv = vec2<f32>(logo_xy.x * 0.5 + 0.5, 0.5 - logo_xy.y * 0.5);
    return textureSample(logo_tex, logo_sampler, uv);
}
