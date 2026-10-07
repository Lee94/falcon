/**
 * 终端滚动条的纯函数层（ADR 0019）。
 *
 * 终端的滚动发生在宿主机的 zellij 里，前端本地没有 scrollback（见 ADR 0019 的背景）：
 * 位置由服务端问 zellij 里的插件得来，单位是 zellij 的显示行。这里只管把它画成滑块、
 * 把拖到的像素换回行数，原生客户端（falcon-core 的 term_scroll.rs）照同一口径移植。
 */

/** 服务端推来的 `{type:"scroll"}`，见 shared 的 ServerMessage */
export interface ScrollState {
  /** 视口下方的行数，0 = 在底部 */
  position: number;
  /** 视口上方 + 下方的行数，0 = 没有可滚的历史 */
  length: number;
  /** 视口高度 */
  rows: number;
}

export interface ThumbGeometry {
  top: number;
  height: number;
}

/** 滑块最矮多少像素：一万行历史时按比例只剩一两个像素，抓不住 */
export const MIN_THUMB_PX = 24;

/**
 * 滑块在轨道里的位置与高度（像素）。没有可滚的历史时为 null——不画滑块。
 *
 * 高度按「视口 / (历史 + 视口)」的比例；位置按视口上方的行数在「轨道 − 滑块」这段里
 * 线性摆：在最上面时贴顶，在底部（position 0）时贴底。夹了最小高度以后仍然两端对得上。
 */
export function thumbGeometry(
  s: ScrollState,
  trackPx: number,
  minThumb = MIN_THUMB_PX
): ThumbGeometry | null {
  if (s.length <= 0 || s.rows <= 0 || trackPx <= 0) return null;
  const height = Math.min(trackPx, Math.max(minThumb, (s.rows / (s.length + s.rows)) * trackPx));
  const above = s.length - clamp(s.position, 0, s.length);
  return { top: (above / s.length) * (trackPx - height), height };
}

/** thumbGeometry 的反函数：滑块顶边拖到 topPx 时对应的 position（视口下方的行数） */
export function positionForThumbTop(
  s: ScrollState,
  trackPx: number,
  thumbPx: number,
  topPx: number
): number {
  const span = trackPx - thumbPx;
  if (s.length <= 0 || span <= 0) return 0;
  const above = Math.round((clamp(topPx, 0, span) / span) * s.length);
  return s.length - above;
}

/**
 * 输入里有没有滚轮报文（SGR 鼠标编码的按键 64 / 65，带不带修饰位都算）。zellij 开着
 * 鼠标上报，滚轮与触摸惯性滚动最后都变成这种报文发出去——不论 xterm 还是 rio 引擎，
 * 在这里看一眼就知道「用户在滚」，不必分别去挂各引擎的滚轮事件。
 */
export function hasWheelReport(data: string): boolean {
  // 按键码 64–67 是滚轮上下左右，再加 4/8/16 是 Shift/Alt/Ctrl，最大 95。滚轮只有按下（M）
  return /\x1b\[<(?:6[4-9]|[7-8]\d|9[0-5]);\d+;\d+M/.test(data);
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}
