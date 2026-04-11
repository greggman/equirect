struct Camera {
  projection: mat4x4f,
  view: mat4x4f,
}
@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexOut {
  @builtin(position) pos: vec4f,
  @location(0) color: vec4f,
}

@vertex
fn vertexMain(@builtin(vertex_index) vert_index: u32,
              @builtin(instance_index) instance: u32) -> VertexOut {
  var pos = array<vec4f, 3>(
    vec4f(0.0, 0.25, -0.5, 1),
    vec4f(-0.25, -0.25, -0.5, 1),
    vec4f(0.25, -0.25, -0.5, 1)
  );

  var color = array<vec4f, 3>(
    vec4f(1, 0, 0, 1),
    vec4f(0, 1, 0, 1),
    vec4f(0, 0, 1, 1)
  );

  // Give each instance a small offset to help with the sense of depth.
  let instancePos = pos[vert_index] + vec4f(0, 0, f32(instance) * -0.1, 0);
  let posOut = camera.projection * camera.view * instancePos;

  return VertexOut(posOut, color[vert_index]);
}

@fragment
fn fragmentMain(in: VertexOut) -> @location(0) vec4f {
  return in.color;
}