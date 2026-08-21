/**
 * 终端 VT 模式跟踪。
 *
 * Viewer 重同步（WS 重连 / 新开页面 / 背压 lagged 后的 resync）走的是
 * term.reset() + RingBuffer 快照回放，而 reset 会清掉 xterm.js 的全部
 * DEC 私有模式。Zellij 客户端只在 attach 那一刻设置一次 ?1049 / ?1002 /
 * ?1006 等模式，RingBuffer 只留最近 4MB——输出多的会话很快把这些字节挤出
 * 窗口，此后每次回放都让该 Viewer 永久失去 mouse tracking / bracketed
 * paste / 方向键编码：滚轮彻底失效，多行粘贴被逐行直接执行。
 *
 * 所以在服务端全程跟踪输出流里的模式变化，回放时把「重建当前模式」的
 * 序列拼在快照前面。语义对齐 xterm.js 的 setModePrivate / resetModePrivate /
 * softReset——客户端只有它，别的终端方言不重要：
 * - 鼠标协议（?9/?1000/?1002/?1003）与编码（?1006/?1016）是单值状态，
 *   后设的赢；关掉其中任意一个号都会把整个状态清空（xterm.js 就是这么写的）；
 * - alt-screen 记住是哪个变体（?47/?1047/?1049）开的，回放用同一个号；
 * - RIS（ESC c）全部回默认；DECSTR（CSI ! p）只重置键盘/光标类布尔模式，
 *   不碰鼠标与 alt-screen（对齐 xterm.js softReset → CoreService.reset）。
 *
 * 正确性依据：模式操作是幂等的赋值。快照里残留的模式序列是真实历史的
 * 后缀，前缀先把状态摆到「当前值」，后缀重放只会把每个模式再赋一次
 * 相同的终值，结果恒等于跟踪到的当前状态——不管环里被挤掉了多少。
 *
 * 另跟踪 ?2031（亮暗主题变化通知订阅），但用途不同：不进回放前缀
 * （xterm.js 不实现它），只作为「主题切换时能不能向 PTY 注入
 * CSI ?997;1/2 n 通知」的开关——没订阅的程序会把这串字节当键盘输入。
 * Zellij client 一启动就会订；plain-pty 会话则看前台程序自己
 * （Claude Code 的 auto 主题会订，实测 2.1.237）。
 */

/** 布尔型私有模式及其默认值（fresh xterm.js / term.reset() 之后的取值）。 */
const BOOL_MODE_DEFAULTS: ReadonlyMap<number, boolean> = new Map([
  [1, false], // DECCKM 应用光标键——丢了它 vim / less 里按方向键会打出 ABCD
  [6, false], // DECOM 起点模式
  [7, true], // DECAWM 自动回绕
  [25, true], // DECTCEM 光标可见
  [45, false], // 反向回绕
  [66, false], // DECNKM 应用小键盘（ESC = / ESC > 折算到同一状态）
  [1004, false], // focus 上报
  [2004, false], // bracketed paste——丢了它多行粘贴会被 shell 逐行执行
]);

/** 解析器状态。不用 const enum：isolatedModules 下由打包器逐文件编译。 */
const GROUND = 0;
const ESC = 1;
const ESC_INTERMEDIATE = 2;
const CSI = 3;
const STR = 4;
const STR_ESC = 5;

/** CSI 参数收集上限。真实模式序列不过十几字节，超长的只可能是畸形流。 */
const CSI_PARAM_MAX = 64;

export class TermModeTracker {
  private bools = new Map(BOOL_MODE_DEFAULTS);
  /** 0 = NONE；否则是最后一次 h 的协议号 */
  private mouseProtocol = 0;
  /** 0 = DEFAULT；否则 1006（SGR）/ 1016（SGR_PIXELS） */
  private mouseEncoding = 0;
  /** 0 = normal buffer；否则是打开 alt-screen 用的那个号 */
  private altScreen = 0;
  /** ?2031 亮暗通知订阅。不进 prefix()，只喂给 997 注入的 gating。 */
  private notify2031 = false;

  /** 是否有程序订阅了亮暗主题变化通知（DECSET 2031）。 */
  get themeNotify(): boolean {
    return this.notify2031;
  }

  private state = GROUND;
  private csiParams = "";

  /**
   * 喂入一段 PTY 输出。序列可以在任意字节处被 chunk 边界切开，
   * 状态机跨调用续接。热路径：文本段用 indexOf 整段跳过，不逐字符走。
   */
  track(data: string): void {
    let i = 0;
    const n = data.length;
    while (i < n) {
      switch (this.state) {
        case GROUND: {
          const esc = data.indexOf("\x1b", i);
          if (esc === -1) return;
          i = esc + 1;
          this.state = ESC;
          break;
        }
        case ESC: {
          const c = data[i++]!;
          if (c === "[") {
            this.state = CSI;
            this.csiParams = "";
          } else if (c === "]" || c === "P" || c === "X" || c === "^" || c === "_") {
            // OSC / DCS / SOS / PM / APC：载荷不解析，吃到 BEL 或 ST 为止
            this.state = STR;
          } else if (c === "c") {
            this.hardReset();
            this.state = GROUND;
          } else if (c === "=") {
            this.bools.set(66, true);
            this.state = GROUND;
          } else if (c === ">") {
            this.bools.set(66, false);
            this.state = GROUND;
          } else if (c >= " " && c <= "/") {
            // 字符集指定等 ESC + 中间字节 + final 的形态
            this.state = ESC_INTERMEDIATE;
          } else if (c !== "\x1b") {
            // 连续 ESC 停在本态重新开始；其余单字符 ESC 序列直接结束
            this.state = GROUND;
          }
          break;
        }
        case ESC_INTERMEDIATE: {
          const c = data[i++]!;
          if (!(c >= " " && c <= "/")) this.state = GROUND;
          break;
        }
        case CSI: {
          while (i < n) {
            const c = data[i++]!;
            const code = c.charCodeAt(0);
            if (code >= 0x40 && code <= 0x7e) {
              this.applyCsi(this.csiParams, c);
              this.state = GROUND;
              break;
            }
            if (code === 0x1b) {
              // 序列被新的 ESC 打断（对齐 VT 解析器的 abort 行为）
              this.state = ESC;
              break;
            }
            if (code === 0x18 || code === 0x1a) {
              // CAN / SUB 中止序列
              this.state = GROUND;
              break;
            }
            if (this.csiParams.length < CSI_PARAM_MAX) this.csiParams += c;
          }
          break;
        }
        case STR: {
          const bel = data.indexOf("\x07", i);
          const esc = data.indexOf("\x1b", i);
          if (bel === -1 && esc === -1) return; // 整段都是载荷
          if (bel !== -1 && (esc === -1 || bel < esc)) {
            i = bel + 1;
            this.state = GROUND;
          } else {
            i = esc + 1;
            this.state = STR_ESC;
          }
          break;
        }
        case STR_ESC: {
          const c = data[i++]!;
          if (c === "\\") this.state = GROUND; // ST
          else if (c !== "\x1b") this.state = STR; // 载荷里的孤立 ESC
          break;
        }
      }
    }
  }

  /** 回放前缀：让 term.reset() 后的 xterm 回到跟踪到的当前模式。 */
  prefix(): string {
    let out = "";
    // alt-screen 放最前：快照内容要画进正确的缓冲区
    if (this.altScreen) out += `\x1b[?${this.altScreen}h`;
    for (const [mode, def] of BOOL_MODE_DEFAULTS) {
      const cur = this.bools.get(mode)!;
      if (cur !== def) out += cur ? `\x1b[?${mode}h` : `\x1b[?${mode}l`;
    }
    if (this.mouseProtocol) out += `\x1b[?${this.mouseProtocol}h`;
    if (this.mouseEncoding) out += `\x1b[?${this.mouseEncoding}h`;
    return out;
  }

  private applyCsi(params: string, final: string): void {
    if (final === "p") {
      if (params === "!") this.softReset();
      return;
    }
    if ((final !== "h" && final !== "l") || params.charCodeAt(0) !== 0x3f /* ? */) {
      return;
    }
    const set = final === "h";
    for (const part of params.slice(1).split(";")) {
      const mode = Number(part);
      if (!Number.isInteger(mode) || mode <= 0) continue;
      switch (mode) {
        case 9:
        case 1000:
        case 1002:
        case 1003:
          this.mouseProtocol = set ? mode : 0;
          break;
        case 1006:
        case 1016:
          this.mouseEncoding = set ? mode : 0;
          break;
        case 47:
        case 1047:
        case 1049:
          this.altScreen = set ? mode : 0;
          break;
        case 2031:
          this.notify2031 = set;
          break;
        default:
          if (BOOL_MODE_DEFAULTS.has(mode)) this.bools.set(mode, set);
      }
    }
  }

  private hardReset(): void {
    this.bools = new Map(BOOL_MODE_DEFAULTS);
    this.mouseProtocol = 0;
    this.mouseEncoding = 0;
    this.altScreen = 0;
    this.notify2031 = false;
  }

  /** DECSTR：xterm.js softReset 恰好把跟踪的布尔模式全部回默认，鼠标与 alt-screen 不动。 */
  private softReset(): void {
    this.bools = new Map(BOOL_MODE_DEFAULTS);
  }
}
