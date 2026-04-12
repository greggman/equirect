// Full-screen blit: copies video_texture to the current render target.
// No vertex buffer — uses a built-in oversized triangle.

@group(0) @binding(0) var src_tex:     texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

struct VertexOut {
    @builtin(position) pos: vec4f,
    @location(0)       uv:  vec2f,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOut {
    // One big triangle covering the whole screen (NDC).
    // Clip-space Y is +1 at top; texture V is 0 at top.
    //   vi=0: (-1,-1)  uv=(0,1)
    //   vi=1: ( 3,-1)  uv=(2,1)
    //   vi=2: (-1, 3)  uv=(0,-1)
    let x  = select(-1.0,  3.0, vi == 1u);
    let y  = select(-1.0,  3.0, vi == 2u);
    let u  = (x + 1.0) * 0.5;
    let v  = 1.0 - (y + 1.0) * 0.5;
    var out: VertexOut;
    out.pos = vec4f(x, y, 0.0, 1.0);
    out.uv  = vec2f(u, v);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4f {
    return textureSample(src_tex, src_sampler, in.uv);
}
