// Shader for fisheye and equirect sphere modes (shader-path fallback).
// Flat / XR-layer modes bypass this entirely.

struct Camera {
    projection: mat4x4f,
    view:       mat4x4f,
}
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var video_texture: texture_2d<f32>;
@group(1) @binding(1) var video_sampler: sampler;
@group(1) @binding(2) var uv_texture:    texture_2d<f32>;  // NV12 chroma (UV) plane

struct VideoParams {
    uv_offset: vec2f,  // stereo crop origin
    uv_scale:  vec2f,  // stereo crop scale
    // 0 = flat pass-through (vertex UV, only used when no XR layer)
    // 1 = fisheye 180°
    // 2 = dual-fisheye 360°
    // 3 = equirect 180°
    // 4 = equirect 360°
    mode:      u32,
    inv_zoom:  f32,    // 1/zoom
    // 0 = BGRA texture, 1 = NV12 (Y in video_texture, UV in uv_texture)
    pixel_fmt: u32,
    _pad0:     u32,
}
@group(2) @binding(0) var<uniform> params: VideoParams;

// ── vertex shaders ────────────────────────────────────────────────────────────

struct VertexIn {
    @location(0) position: vec3f,
    @location(1) uv:       vec2f,
}

struct VertexOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) uv:  vec2f,
}

/// Standard vertex — flat quad for mode 0.
@vertex fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip_pos = camera.projection * camera.view * vec4f(in.position, 1.0);
    out.uv = in.uv;
    return out;
}

/// Fullscreen-triangle — used for modes 1-4 (fisheye + equirect).
/// Passes clip-space xy as uv so the fragment shader can reconstruct ray directions.
@vertex fn vs_equirect(@builtin(vertex_index) vi: u32) -> VertexOut {
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    var out: VertexOut;
    // z = 1.0 places the skybox at the far plane so VR reprojection works correctly.
    out.clip_pos = vec4f(x, y, 1.0, 1.0);
    out.uv = vec2f(x, y);
    return out;
}

// ── fragment shader ───────────────────────────────────────────────────────────

const PI: f32 = 3.14159265358979323846;

/// Reconstruct the world-space unit direction for a fragment at clip-space position
/// `clip_xy`.  Uses the camera projection to unproject to view space, then the
/// rotation part of the view matrix (translation ignored) to reach world space.
fn world_dir(clip_xy: vec2f) -> vec3f {
    let P = camera.projection;
    let raw = vec3f(
        clip_xy.x / P[0][0] * params.inv_zoom,
        clip_xy.y / P[1][1] * params.inv_zoom,
        -1.0,
    );
    let R_inv = transpose(mat3x3f(
        camera.view[0].xyz,
        camera.view[1].xyz,
        camera.view[2].xyz,
    ));
    return normalize(R_inv * raw);
}

/// sRGB EOTF: gamma-compressed video value → linear light.
/// Applied after YCbCr conversion so the output matches what Bgra8UnormSrgb
/// texture sampling would produce (linear values for an sRGB render target).
fn srgb_to_linear(x: f32) -> f32 {
    return select(x / 12.92, pow((x + 0.055) / 1.055, 2.4), x > 0.04045);
}

/// BT.709 studio-swing YCbCr → linear RGBA.
/// Y  is sampled from `video_texture` (R8Unorm).
/// UV is sampled from `uv_texture`   (Rg8Unorm, R=Cb, G=Cr).
/// Output is linearised so writing to an sRGB render target is correct.
fn nv12_to_rgba(uv: vec2f) -> vec4f {
    let y_raw  = textureSample(video_texture, video_sampler, uv).r * 255.0;
    let uv_raw = textureSample(uv_texture,    video_sampler, uv).rg * 255.0;
    let yp = y_raw    - 16.0;
    let cb = uv_raw.r - 128.0;
    let cr = uv_raw.g - 128.0;
    // BT.709 integer formula scaled to [0,1]: divide by 256*255 = 65280.
    let r = (298.0 * yp + 459.0 * cr + 128.0) / 65280.0;
    let g = (298.0 * yp -  55.0 * cb - 136.0 * cr + 128.0) / 65280.0;
    let b = (298.0 * yp + 541.0 * cb + 128.0) / 65280.0;
    return vec4f(
        srgb_to_linear(clamp(r, 0.0, 1.0)),
        srgb_to_linear(clamp(g, 0.0, 1.0)),
        srgb_to_linear(clamp(b, 0.0, 1.0)),
        1.0,
    );
}

@fragment fn fs_main(in: VertexOut) -> @location(0) vec4f {
    var uv:         vec2f;
    var is_outside: bool = false;

    if params.mode == 1u {
        // ── Fisheye 180° ──────────────────────────────────────────────────
        let d         = world_dir(in.uv);
        let cos_theta = clamp(dot(d, vec3f(0.0, 0.0, -1.0)), -1.0, 1.0);
        let r         = acos(cos_theta) / radians(90.0);
        is_outside    = r > 1.0;
        let phi       = atan2(d.y, d.x);
        uv = vec2f(0.5 + 0.5 * r * cos(phi),
                   0.5 - 0.5 * r * sin(phi));

    } else if params.mode == 2u {
        // ── Dual fisheye 360° ─────────────────────────────────────────────
        let d  = world_dir(in.uv);
        var d_rel:  vec3f;
        var u_base: f32;
        if d.z <= 0.0 { d_rel = d;                        u_base = 0.0; }
        else          { d_rel = vec3f(-d.x, d.y, -d.z);   u_base = 0.5; }
        let r      = acos(clamp(dot(d_rel, vec3f(0.0, 0.0, -1.0)), -1.0, 1.0)) / radians(90.0);
        is_outside = r > 1.0;
        let phi    = atan2(d_rel.y, d_rel.x);
        uv = vec2f(u_base + 0.25 + 0.25 * r * cos(phi),
                             0.5  - 0.5  * r * sin(phi));

    } else if params.mode == 3u {
        // ── Equirect 180° ─────────────────────────────────────────────────
        let d  = world_dir(in.uv);
        is_outside = d.z >= 0.0;
        uv = vec2f(
            atan2(d.x, -d.z) / PI + 0.5,
            0.5 - asin(clamp(d.y, -1.0, 1.0)) / PI,
        );

    } else if params.mode == 4u {
        // ── Equirect 360° ─────────────────────────────────────────────────
        let d  = world_dir(in.uv);
        uv = vec2f(
            atan2(d.x, -d.z) / (2.0 * PI) + 0.5,
            0.5 - asin(clamp(d.y, -1.0, 1.0)) / PI,
        );

    } else {
        // ── Flat / pass-through (mode 0) ──────────────────────────────────
        uv = in.uv;
    }

    // Apply stereo crop.  Both texture samples must be in uniform control flow.
    let final_uv  = params.uv_offset + uv * params.uv_scale;
    let bgra_col  = textureSample(video_texture, video_sampler, final_uv);
    let nv12_col  = nv12_to_rgba(final_uv);
    let colour    = select(bgra_col, nv12_col, params.pixel_fmt == 1u);
    return select(colour, vec4f(0.0, 0.0, 0.0, 1.0), is_outside);
}
