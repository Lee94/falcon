/** 面板内的下钻栈。视图与全景视图共用一页，只是取数接口不同 */
export type Drill =
  | {
      kind: "view";
      spaceKey: string;
      spaceName?: string;
      viewId: string;
      label: string;
      multi: boolean;
      typeKey?: string;
      typeName?: string;
      url?: string;
    }
  | {
      kind: "item";
      spaceKey: string;
      spaceName?: string;
      id: string;
      title: string;
      typeKey?: string;
      url?: string;
    };

/** 每层都要重新取一次数，记太深既没人回得去也白撑 localStorage */
const MAX_DEPTH = 8;

function str(value: unknown): string | undefined {
  return typeof value === "string" && value ? value : undefined;
}

/** 取不到的可选字段整个不写，恢复出来的层才和存进去的那份逐字段相等 */
function compact<T extends object>(layer: T): T {
  return Object.fromEntries(Object.entries(layer).filter(([, v]) => v !== undefined)) as T;
}

/** 栈里存的全是 CLI 要的标识与显示名，没有正文；坏一层就当没有，绝不半信半疑地放行 */
function parseLayer(value: unknown): Drill | null {
  if (typeof value !== "object" || value === null) return null;
  const v = value as Record<string, unknown>;
  const spaceKey = str(v.spaceKey);
  if (!spaceKey) return null;
  const shared = { spaceKey, spaceName: str(v.spaceName), typeKey: str(v.typeKey), url: str(v.url) };
  if (v.kind === "item") {
    const id = str(v.id);
    // title 只是打开详情前的占位，详情回来会覆盖，空着也能恢复
    return id ? compact({ kind: "item" as const, ...shared, id, title: str(v.title) ?? "" }) : null;
  }
  if (v.kind === "view") {
    const viewId = str(v.viewId);
    return viewId
      ? compact({
          kind: "view" as const,
          ...shared,
          viewId,
          label: str(v.label) ?? viewId,
          multi: v.multi === true,
          typeName: str(v.typeName),
        })
      : null;
  }
  return null;
}

/** 面板重开时恢复上次停的位置；存储被改坏或换了版本都只是回到根页，不弹错 */
export function parseDrill(raw: string | null): Drill[] {
  if (!raw) return [];
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(value)) return [];
  const stack: Drill[] = [];
  for (const layer of value.slice(0, MAX_DEPTH)) {
    const parsed = parseLayer(layer);
    // 中间断一层，上面几层的返回路径就不对了，只恢复到断点为止
    if (!parsed) break;
    stack.push(parsed);
  }
  return stack;
}

export function serializeDrill(stack: Drill[]): string {
  return JSON.stringify(stack.slice(0, MAX_DEPTH));
}
