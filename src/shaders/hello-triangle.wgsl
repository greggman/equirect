struct VOut {
  @builtin(position) pos: vec4f,
  @location(0) color: vec4f,
};

@vertex fn vs(
    @builtin(vertex_index) vertex_index: u32
) -> VOut {
    var positions = array<vec2f, 3>(
        vec2f(0.0, 0.5),
        vec2f(-0.5, -0.5),
        vec2f(0.5, -0.5)
    );
    let position = positions[vertex_index];
    return VOut(
      vec4f(position, 0.0, 1.0),
      vec4f(position * 2.0 + 1.0, 0, 1),
    );

}

@fragment fn fs(in: VOut) -> @location(0) vec4f {
    return in.color;
}
