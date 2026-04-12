// Simple textured-quad shader for UI panels.
// No VideoParams — panels are always rendered flat.

struct Camera {
    projection: mat4x4f,
    view:       mat4x4f,
}
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var panel_texture: texture_2d<f32>;
@group(1) @binding(1) var panel_sampler: sampler;

struct VertexIn {
    @location(0) position: vec3f,
    @location(1) uv:       vec2f,
}

struct VertexOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip_pos = camera.projection * camera.view * vec4f(in.position, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4f {
    return textureSample(panel_texture, panel_sampler, in.uv);
}
