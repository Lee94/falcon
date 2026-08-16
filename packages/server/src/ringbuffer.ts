/** 按字节数封顶的输出环形缓冲，用于 Viewer 重连时回放 Scrollback。 */
export class RingBuffer {
  private chunks: string[] = [];
  private bytes = 0;

  constructor(private maxBytes = 1024 * 1024) {}

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
