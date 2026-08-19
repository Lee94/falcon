/** 按字节数封顶的输出环形缓冲，用于 Viewer 重连时回放 Scrollback。 */
export class RingBuffer {
  private chunks: string[] = [];
  private bytes = 0;

  // 4MB：要装得下 dump-screen 的整份快照（scroll_buffer_size 10000 行、
  // 含 ANSI 时约 1-2MB）再留出后续实时输出的余量。快照是单个 chunk，
  // 上限太小的话它会在之后第一次触顶时被整块淘汰，历史瞬间清零。
  constructor(private maxBytes = 4 * 1024 * 1024) {}

  append(data: string) {
    this.chunks.push(data);
    this.bytes += Buffer.byteLength(data);
    while (this.bytes > this.maxBytes && this.chunks.length > 1) {
      const removed = this.chunks.shift()!;
      this.bytes -= Buffer.byteLength(removed);
    }
  }

  snapshot(): string {
    return this.chunks.join("");
  }

  reset(data?: string) {
    this.chunks = [];
    this.bytes = 0;
    if (data) this.append(data);
  }
}
