// TS 6 起 noUncheckedSideEffectImports 默认打开，`import "./styles.css"` 这类副作用
// import 也必须解析得到声明。tsconfig 刻意没带 vite/client 类型（见 lib/rio/gpu.ts），
// 这里只补 CSS 这一条。
declare module "*.css";
