// Simple vertex-colour shader used for the controller pointer rays.

struct Camera {
    projection: mat4x4<f32>,
    view:       mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> camera: Camera;

struct VIn {
    @location(0) position: vec3<f32>,
    @location(1) color:    vec4<f32>,
}
struct VOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0)       color:    vec4<f32>,
}

@vertex
fn vs_main(v: VIn) -> VOut {
    var out: VOut;
    out.clip_pos = camera.projection * camera.view * vec4<f32>(v.position, 1.0);
    out.color    = v.color;
    return out;
}

@fragment
fn fs_main(v: VOut) -> @location(0) vec4<f32> {
    return v.color;
}
