// Full-screen blit: copies a video frame (BGRA or NV12) to the current render target.
// No vertex buffer — uses a built-in oversized triangle.

@group(0) @binding(0) var src_tex:     texture_2d<f32>;  // BGRA or NV12 Y plane
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var uv_tex:      texture_2d<f32>;  // NV12 UV plane (unused for BGRA)

struct BlitParams {
    // 0 = BGRA texture, 1 = NV12 (Y in src_tex, UV in uv_tex)
    pixel_fmt: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}
@group(1) @binding(0) var<uniform> blit_params: BlitParams;

struct VertexOut {
    @builtin(position) pos: vec4f,
    @location(0)       uv:  vec2f,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOut {
    let x  = select(-1.0,  3.0, vi == 1u);
    let y  = select(-1.0,  3.0, vi == 2u);
    let u  = (x + 1.0) * 0.5;
    let v  = 1.0 - (y + 1.0) * 0.5;
    var out: VertexOut;
    out.pos = vec4f(x, y, 0.0, 1.0);
    out.uv  = vec2f(u, v);
    return out;
}

/// sRGB EOTF: gamma-compressed video value → linear light.
/// Applied after YCbCr conversion so the output matches what Bgra8UnormSrgb
/// texture sampling would produce (linear values for an sRGB render target).
fn srgb_to_linear(x: f32) -> f32 {
    return select(x / 12.92, pow((x + 0.055) / 1.055, 2.4), x > 0.04045);
}

/// BT.709 studio-swing YCbCr → linear RGBA.
/// Y  is sampled from `src_tex` (R8Unorm).
/// UV is sampled from `uv_tex`  (Rg8Unorm, R=Cb, G=Cr).
/// Output is linearised so writing to an sRGB render target is correct.
fn nv12_to_rgba(uv: vec2f) -> vec4f {
    let y_raw  = textureSample(src_tex, src_sampler, uv).r * 255.0;
    let uv_raw = textureSample(uv_tex,  src_sampler, uv).rg * 255.0;
    let yp = y_raw    - 16.0;
    let cb = uv_raw.r - 128.0;
    let cr = uv_raw.g - 128.0;
    let r  = (298.0 * yp + 459.0 * cr + 128.0) / 65280.0;
    let g  = (298.0 * yp -  55.0 * cb - 136.0 * cr + 128.0) / 65280.0;
    let b  = (298.0 * yp + 541.0 * cb + 128.0) / 65280.0;
    return vec4f(
        srgb_to_linear(clamp(r, 0.0, 1.0)),
        srgb_to_linear(clamp(g, 0.0, 1.0)),
        srgb_to_linear(clamp(b, 0.0, 1.0)),
        1.0,
    );
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4f {
    // Both samples must be in uniform control flow; select the result.
    let bgra = textureSample(src_tex, src_sampler, in.uv);
    let nv12 = nv12_to_rgba(in.uv);
    return select(bgra, nv12, blit_params.pixel_fmt == 1u);
}
