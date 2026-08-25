import { Fragment, useMemo, type ReactNode } from "react";
import { marked, type Token, type Tokens } from "marked";
import { externalHref, resolveRel } from "../lib/mdLink.js";
import { langForFence, splitCodeLines, useHighlight } from "../lib/highlight.js";
import { cn } from "@/lib/utils";
import { CodeLine } from "@/components/common/CodeLine";

/**
 * Markdown 预览。README / CHANGELOG / ADR 这类文件在这里是主要内容，
 * 给一坨等宽源码不如直接渲染出来。
 *
 * **只用 marked 的词法分析，渲染自己做**：marked 的 HTML 输出要配一个消毒器才敢
 * 往 dangerouslySetInnerHTML 里塞，而 token 树映射成 React 元素天然没有这个问题——
 * 文本永远是文本节点，标签只可能是这里白名单里的那些。代价是要自己写映射，
 * 换来的是不必引入第二个依赖，也不必信任消毒器的配置。
 *
 * 内联 HTML 一律按纯文本显示，理由同上：渲染它就等于把 XSS 面重新打开。
 */
export function Markdown({
  text,
  /** 当前文件所在目录（工作目录相对），用来解析文档里的相对链接 */
  dir,
  /** 点开文档里指向仓库内另一个文件的链接 */
  onOpenPath,
}: {
  text: string;
  dir: string;
  onOpenPath?: (path: string) => void;
}) {
  const tokens = useMemo(() => marked.lexer(text), [text]);
  const ctx: Ctx = { dir, onOpenPath };
  return (
    <div className="mx-auto max-w-3xl px-6 py-5 text-sm break-words text-foreground">
      {renderTokens(tokens, ctx)}
    </div>
  );
}

interface Ctx {
  dir: string;
  onOpenPath?: (path: string) => void;
}

function renderTokens(tokens: Token[] | undefined, ctx: Ctx): ReactNode[] {
  return (tokens ?? []).map((tok, i) => <Node key={i} tok={tok} ctx={ctx} />);
}

function Node({ tok, ctx }: { tok: Token; ctx: Ctx }): ReactNode {
  switch (tok.type) {
    case "space":
      return null;

    case "heading": {
      const t = tok as Tokens.Heading;
      const inner = renderTokens(t.tokens, ctx);
      const cls = [
        "mt-6 mb-3 border-b pb-1.5 text-2xl font-semibold",
        "mt-6 mb-3 border-b pb-1 text-xl font-semibold",
        "mt-5 mb-2 text-base font-semibold",
        "mt-4 mb-2 text-sm font-semibold",
        "mt-4 mb-2 text-sm font-semibold text-muted-foreground",
        "mt-4 mb-2 text-xs font-semibold text-muted-foreground",
      ][Math.min(t.depth, 6) - 1];
      // 标题层级要如实映射到 h1–h6，屏幕阅读器靠它跳转
      const H = `h${Math.min(t.depth, 6)}` as "h1";
      return <H className={cn("scroll-mt-4 first:mt-0", cls)}>{inner}</H>;
    }

    case "paragraph":
      return <p className="my-3 leading-7">{renderTokens((tok as Tokens.Paragraph).tokens, ctx)}</p>;

    case "text": {
      const t = tok as Tokens.Text;
      // 列表项里的 text token 自己带内联 token；纯文本的没有
      return t.tokens ? <>{renderTokens(t.tokens, ctx)}</> : <>{t.text}</>;
    }

    case "escape":
      return <>{(tok as Tokens.Escape).text}</>;

    case "code": {
      const t = tok as Tokens.Code;
      return <CodeBlock text={t.text} lang={t.lang} />;
    }

    case "codespan":
      return (
        <code className="rounded bg-muted px-1 py-0.5 font-mono text-[0.9em]">
          {(tok as Tokens.Codespan).text}
        </code>
      );

    case "blockquote":
      return (
        <blockquote className="my-3 border-l-2 pl-3 text-muted-foreground">
          {renderTokens((tok as Tokens.Blockquote).tokens, ctx)}
        </blockquote>
      );

    case "list": {
      const t = tok as Tokens.List;
      const items = t.items.map((item, i) => (
        <li key={i} className={cn(item.task && "list-none")}>
          {item.task && (
            <input
              type="checkbox"
              checked={item.checked === true}
              readOnly
              className="mr-1.5 align-middle"
            />
          )}
          {renderTokens(item.tokens, ctx)}
        </li>
      ));
      const cls = "my-3 space-y-1 pl-6 [&_p]:my-0";
      return t.ordered ? (
        <ol className={cn(cls, "list-decimal")} start={Number(t.start) || 1}>
          {items}
        </ol>
      ) : (
        <ul className={cn(cls, "list-disc")}>{items}</ul>
      );
    }

    case "table": {
      const t = tok as Tokens.Table;
      return (
        <div className="my-3 overflow-x-auto">
          <table className="w-full border-collapse text-[13px]">
            <thead>
              <tr>
                {t.header.map((cell, i) => (
                  <th
                    key={i}
                    className="border px-2 py-1 text-left font-medium"
                    style={{ textAlign: t.align[i] ?? undefined }}
                  >
                    {renderTokens(cell.tokens, ctx)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {t.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((cell, i) => (
                    <td
                      key={i}
                      className="border px-2 py-1 align-top"
                      style={{ textAlign: t.align[i] ?? undefined }}
                    >
                      {renderTokens(cell.tokens, ctx)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    }

    case "hr":
      return <hr className="my-6" />;

    case "strong":
      return <strong className="font-semibold">{renderTokens((tok as Tokens.Strong).tokens, ctx)}</strong>;

    case "em":
      return <em className="italic">{renderTokens((tok as Tokens.Em).tokens, ctx)}</em>;

    case "del":
      return <del className="opacity-70">{renderTokens((tok as Tokens.Del).tokens, ctx)}</del>;

    case "br":
      return <br />;

    case "link": {
      const t = tok as Tokens.Link;
      const inner = renderTokens(t.tokens, ctx);
      return (
        <Link href={t.href} title={t.title ?? undefined} ctx={ctx}>
          {inner}
        </Link>
      );
    }

    case "image": {
      const t = tok as Tokens.Image;
      const external = externalHref(t.href);
      if (external) {
        return <img src={external} alt={t.text} title={t.title ?? undefined} className="my-3 max-w-full rounded" />;
      }
      // 相对图片的字节在宿主机上，浏览器取不到——给一个能点开的占位，
      // 点了就在查看 tab 里打开那张图（后端读回 base64）
      const path = resolveRel(ctx.dir, t.href);
      return (
        <button
          type="button"
          disabled={!path || !ctx.onOpenPath}
          onClick={() => path && ctx.onOpenPath?.(path)}
          className="my-1 inline-flex items-center rounded border border-dashed px-2 py-1 font-mono text-xs text-muted-foreground hover:bg-accent disabled:opacity-60"
          title={t.href}
        >
          {t.text || t.href}
        </button>
      );
    }

    // 内联 / 块级 HTML 一律按原文显示，不解释
    case "html":
      return <span className="font-mono text-xs text-muted-foreground">{(tok as Tokens.HTML).raw}</span>;

    default: {
      const raw = (tok as Tokens.Generic).raw;
      return raw ? <>{raw}</> : null;
    }
  }
}

/**
 * fence 代码块。语言认得出就上语法高亮（异步渐进，token 没到前是纯文本），
 * 认不出保持原样。token 逐行给（useHighlight 的产出天然按行），行间自己补 \n，
 * pre 的 whitespace 会保留它——不引入每行一个块级元素的额外布局。
 */
function CodeBlock({ text, lang }: { text: string; lang?: string }) {
  const hl = useHighlight(text, langForFence(lang));
  const lines = useMemo(() => splitCodeLines(text), [text]);
  return (
    <pre className="code-hl my-3 overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-xs leading-5.5">
      <code>
        {hl
          ? lines.map((line, i) => (
              <Fragment key={i}>
                {i > 0 && "\n"}
                <CodeLine text={line} tokens={hl[i]} />
              </Fragment>
            ))
          : text}
      </code>
    </pre>
  );
}

function Link({
  href,
  title,
  ctx,
  children,
}: {
  href: string;
  title?: string;
  ctx: Ctx;
  children: ReactNode;
}) {
  const cls = "text-primary underline underline-offset-2 hover:opacity-80";
  const external = externalHref(href);
  if (external) {
    return (
      <a href={external} title={title} target="_blank" rel="noreferrer noopener" className={cls}>
        {children}
      </a>
    );
  }
  if (href.startsWith("#")) {
    // 文档内锚点：这里没有目录也没做 id 映射，点了会跳到页面顶部反而更奇怪
    return <span title={title}>{children}</span>;
  }
  const path = resolveRel(ctx.dir, href);
  if (!path || !ctx.onOpenPath) return <span title={href}>{children}</span>;
  return (
    <button type="button" className={cls} title={href} onClick={() => ctx.onOpenPath?.(path)}>
      {children}
    </button>
  );
}
