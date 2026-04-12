// Shader for fisheye and equirect sphere modes (shader-path fallback).
// Flat / XR-layer modes bypass this entirely.

struct Camera {
    projection: mat4x4f,
    view:       mat4x4f,
}
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var video_texture: texture_2d<f32>;
@group(1) @binding(1) var video_sampler: sampler;

struct VideoParams {
    uv_offset: vec2f,  // stereo crop origin  (applied to final UV for modes 1-4)
    uv_scale:  vec2f,  // stereo crop scale   (applied to final UV for modes 1-4)
    // 0 = flat pass-through (vertex UV, only used when no XR layer)
    // 1 = fisheye 180°
    // 2 = dual-fisheye 360°
    // 3 = equirect 180°
    // 4 = equirect 360°
    mode:     u32,
    inv_zoom: f32,  // 1/zoom — scales the unprojected view direction for modes 1-4
    _pad0:    u32,
    _pad1:    u32,
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
/// `params.inv_zoom` narrows the cone (1/zoom > 1 → zoomed in).
fn world_dir(clip_xy: vec2f) -> vec3f {
    let P = camera.projection;
    // Unproject clip position to view-space ray direction.
    // We intentionally ignore P[2][0]/P[2][1] (the asymmetric frustum offset) so that
    // the viewport centre (NDC 0,0) always maps to the forward axis.  Using the full
    // offset shifts the video centre away from the optical axis by 5-10° per eye.
    let raw = vec3f(
        clip_xy.x / P[0][0] * params.inv_zoom,
        clip_xy.y / P[1][1] * params.inv_zoom,
        -1.0,
    );
    // Rotate from view space to world space: R^{-1} = transpose of view's 3×3 block.
    let R_inv = transpose(mat3x3f(
        camera.view[0].xyz,
        camera.view[1].xyz,
        camera.view[2].xyz,
    ));
    return normalize(R_inv * raw);
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
        is_outside = d.z >= 0.0;   // back hemisphere → black
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

    // Apply stereo crop and then sample.  textureSample must be in uniform
    // control flow so it is always called unconditionally.
    let final_uv = params.uv_offset + uv * params.uv_scale;
    let colour   = textureSample(video_texture, video_sampler, final_uv);
    return select(colour, vec4f(0.0, 0.0, 0.0, 1.0), is_outside);
}
