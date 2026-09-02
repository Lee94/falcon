/// <reference types="@webgpu/types" />

// TS 5.9 的 lib.dom 还没有 WebGPU（只有一个 GPUError 接口，与 @webgpu/types 同型可合并）。
// 放三斜线而不是 tsconfig 的 compilerOptions.types：一旦写了 types 数组就会关掉
// @types/* 的自动包含，src 下测试用的 node:test 会立刻失去类型。这份 .d.ts 在
// include: ["src"] 里，全局 GPU* 对整个 web 包可见；WebGPU 相关代码都在本目录，
// 将来升级/移除时和依赖一起走。
