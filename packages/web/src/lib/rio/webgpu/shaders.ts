/**
 * WGSL。写成字符串常量而不是 .wgsl 文件：tsconfig 没有 vite/client 类型，`?raw`
 * 导入会报错，而且两个 shader 加起来不到两百行。
 *
 * bg pass：一个全屏三角形，片元用 floor(pixel / cell) 找到自己的格子，从 cols×rows
 * 的 rgba8unorm 纹理里 textureLoad 一个颜色。零几何、一次 draw。不用 storage buffer
 * 是因为 compat 模式（Android 的 GLES 后端）`maxStorageBuffersInFragmentStage` 可能为 0。
 * block 光标与失焦的空心框也在这里画：光标移动只改 uniform，不脏任何行。
 *
 * text pass：instanced quad，4 顶点 triangle strip 由 vertex_index 生成角点，实例
 * 布局见 frame.ts。图集用 textureLoad 整数取样，不经 sampler。整数 varying 必须
 * @interpolate(flat)，否则编译失败（Safari 报得最严）。颜色全程预乘，blend 是
 * one / one-minus-src-alpha。
 */

export const UNIFORM_BYTES = 80;

/** 与 GridUniforms 一样手工对齐：vec4 成员 16 字节边界，总长 16 的倍数 */
const UNIFORM_STRUCT = /* wgsl */ `
struct Uniforms {
  cell: vec2<u32>,          // 0   物理像素
  grid: vec2<u32>,          // 8   cols, rows
  cursor: vec2<u32>,        // 16  col, row
  cursor_kind: u32,         // 24  0 无 1 block 2 bar 3 underline 4 hollow
  _p0: u32,                 // 28
  cursor_bg: vec4<f32>,     // 32
  cursor_fg: vec4<f32>,     // 48
  canvas: vec2<f32>,        // 64  物理像素
  _p1: vec2<f32>,           // 72
};
@group(0) @binding(0) var<uniform> u: Uniforms;
`;

export const BG_SHADER = /* wgsl */ `
${UNIFORM_STRUCT}
@group(0) @binding(1) var bg_tex: texture_2d<f32>;

struct VsOut {
  @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
  // 全屏三角形：(-1,-1) (3,-1) (-1,3)
  var o: VsOut;
  let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
  let y = f32(vi & 2u) * 2.0 - 1.0;
  o.pos = vec4<f32>(x, y, 0.0, 1.0);
  return o;
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  let px = vec2<u32>(pos.xy);
  let cell = min(px / u.cell, u.grid - vec2<u32>(1u, 1u));
  var c = textureLoad(bg_tex, vec2<i32>(cell), 0);
  if (all(cell == u.cursor)) {
    if (u.cursor_kind == 1u) {
      c = u.cursor_bg;
    } else if (u.cursor_kind == 4u) {
      let local = px - cell * u.cell;
      if (local.x == 0u || local.y == 0u || local.x == u.cell.x - 1u || local.y == u.cell.y - 1u) {
        c = u.cursor_bg;
      }
    }
  }
  return vec4<f32>(c.rgb, 1.0);
}
`;

export const TEXT_SHADER = /* wgsl */ `
${UNIFORM_STRUCT}
@group(1) @binding(0) var gray_tex: texture_2d<f32>;
@group(1) @binding(1) var color_tex: texture_2d<f32>;

const FLAG_COLOR_ATLAS: u32 = 1u;
const FLAG_SOLID: u32 = 2u;

struct Instance {
  @location(0) glyph_pos: vec2<u32>,
  @location(1) glyph_size: vec2<u32>,
  @location(2) bearing: vec2<i32>,
  @location(3) grid_pos: vec2<u32>,
  @location(4) color: vec4<f32>,
  @location(5) flags: u32,
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) @interpolate(flat) glyph_pos: vec2<u32>,
  @location(1) @interpolate(flat) color: vec4<f32>,
  @location(2) @interpolate(flat) flags: u32,
  @location(3) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, in: Instance) -> VsOut {
  // 0 --- 1
  // |     |   strip 顺序 0,1,2,3
  // 2 --- 3
  let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
  let size = vec2<f32>(in.glyph_size);
  let origin = vec2<f32>(in.grid_pos * u.cell) + vec2<f32>(in.bearing);
  let p = origin + corner * size;
  var o: VsOut;
  o.pos = vec4<f32>(p.x / u.canvas.x * 2.0 - 1.0, 1.0 - p.y / u.canvas.y * 2.0, 0.0, 1.0);
  o.glyph_pos = in.glyph_pos;
  o.uv = corner * size;
  o.flags = in.flags;
  var color = in.color;
  // block 光标下的字形反色
  if (u.cursor_kind == 1u && all(in.grid_pos == u.cursor)) {
    color = u.cursor_fg;
  }
  o.color = color;
  return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  if ((in.flags & FLAG_SOLID) != 0u) {
    return vec4<f32>(in.color.rgb, 1.0);
  }
  // 整个坐标系都是物理整数，quad 与位图逐像素对齐，floor(uv) 就是纹素
  let tc = vec2<i32>(in.glyph_pos) + vec2<i32>(floor(in.uv));
  if ((in.flags & FLAG_COLOR_ATLAS) != 0u) {
    return textureLoad(color_tex, tc, 0);
  }
  let a = textureLoad(gray_tex, tc, 0).r;
  return vec4<f32>(in.color.rgb * a, a);
}
`;

/** 与 frame.ts 的 packInstance 逐字段对应 */
export const INSTANCE_VERTEX_LAYOUT: GPUVertexBufferLayout = {
  arrayStride: 24,
  stepMode: "instance",
  attributes: [
    { shaderLocation: 0, offset: 0, format: "uint16x2" },
    { shaderLocation: 1, offset: 4, format: "uint16x2" },
    { shaderLocation: 2, offset: 8, format: "sint16x2" },
    { shaderLocation: 3, offset: 12, format: "uint16x2" },
    { shaderLocation: 4, offset: 16, format: "unorm8x4" },
    { shaderLocation: 5, offset: 20, format: "uint32" },
  ],
};
