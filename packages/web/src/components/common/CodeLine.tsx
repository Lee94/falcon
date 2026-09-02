import type { CSSProperties } from "react";
import type { HlLine } from "@/lib/highlight";

/**
 * 一行高亮代码。token 还没到（shiki 在加载 / 逐块跑）就退回纯文本——
 * 高亮永远是增强，不是渲染的前提，所以这里没有 loading 态。
 *
 * token 的 htmlStyle 是 `color: var(--shiki-token-xxx)`（shiki 的 css-variables 主题），
 * 变量值由当前主题派生并写在 <html> 上（lib/theme/derive.ts），换主题不用重新 tokenize。
 */
export function CodeLine({ text, tokens }: { text: string; tokens?: HlLine }) {
  if (!tokens?.length) return <>{text}</>;
  return (
    <>
      {tokens.map((t, i) => (
        <span key={i} style={t.htmlStyle as CSSProperties | undefined}>
          {t.content}
        </span>
      ))}
    </>
  );
}
