/**
 * 终端里粘贴 / 拖入图片的判定。
 *
 * 图片走上传 → 宿主机落盘 → 把路径粘进输入框（Claude Code 认输入框里的
 * 图片路径）。判定规则：剪贴板里**有文本就贴文本**——Excel / 网页复制经常
 * 同时带 text 与渲染好的位图，用户要的是文本；截图与复制的图片文件没有
 * text/plain，才当图片处理。
 */

/** 结构化到刚好能测的最小面；DataTransfer 天然满足 */
export interface DataTransferLike {
  items?: ArrayLike<{ kind: string; type: string; getAsFile(): File | null }>;
  files?: ArrayLike<File>;
  getData?(type: string): string;
}

function isImageType(type: string): boolean {
  return /^image\//.test(type);
}

/** 粘贴事件里应当按图片处理的文件；应走原有文本粘贴时返回 null */
export function imageFromClipboard(dt: DataTransferLike | null): File | null {
  if (!dt?.items) return null;
  if (dt.getData?.("text/plain")) return null;
  for (let i = 0; i < dt.items.length; i++) {
    const item = dt.items[i];
    if (item.kind === "file" && isImageType(item.type)) {
      return item.getAsFile();
    }
  }
  return null;
}

/** 拖放里的图片文件；拖动是显式动作，不看文本、可以多张 */
export function imagesFromDrop(dt: DataTransferLike | null): File[] {
  if (!dt?.files) return [];
  return Array.from(dt.files).filter((f) => isImageType(f.type));
}

/**
 * 粘进终端前给含空白的路径包上双引号（Windows 用户名带空格很常见），
 * 对齐终端拖拽文件的习惯——Claude Code 按这个约定剥引号。
 */
export function quoteForPrompt(p: string): string {
  return /\s/.test(p) ? `"${p}"` : p;
}
