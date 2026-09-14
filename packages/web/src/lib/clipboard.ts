/**
 * Safari 的剪贴板授权不能跨网络 await：在用户点击栈里提交 ClipboardItem，
 * 再让浏览器等待正文。旧浏览器只能在正文就绪后尝试 writeText，权限错误交给调用者。
 */
export async function writeClipboardText(
  load: () => Promise<string>,
  clipboard: Pick<Clipboard, "write" | "writeText"> = navigator.clipboard,
  Item: typeof ClipboardItem | undefined = globalThis.ClipboardItem
): Promise<void> {
  if (Item && clipboard?.write) {
    const blob = load().then((text) => new Blob([text], { type: "text/plain" }));
    // 权限立即拒绝时网络任务仍可能失败；避免留下未处理的 Promise rejection。
    void blob.catch(() => {});
    await clipboard.write([new Item({ "text/plain": blob })]);
  } else {
    const text = await load();
    await clipboard.writeText(text);
  }
}
