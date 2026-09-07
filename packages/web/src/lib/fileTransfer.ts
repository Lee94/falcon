/**
 * 文件面板的下载 / 上传在浏览器这一侧的两件小事：触发"存到本地"、弹系统文件选择框。
 * 与服务端的往返在 api.ts（downloadUrl / uploadFile）。
 */

/**
 * 让浏览器把一个同源地址当附件下载。`<a download>` 点击是最省事的办法：
 * 不换页、不开新标签、不经 fetch 把整个文件攒进内存——浏览器自己的下载管理器
 * 接手，进度与取消都有。响应带 Content-Disposition，文件名以那边的为准
 * （download 属性留空就是这个意思）。
 */
export function triggerDownload(url: string): void {
  const a = document.createElement("a");
  a.href = url;
  a.download = "";
  a.rel = "noopener";
  a.style.display = "none";
  document.body.append(a);
  a.click();
  a.remove();
}

/**
 * 弹系统文件选择框，返回选中的文件；取消时返回空数组。
 *
 * `cancel` 事件是较新的标准（Chrome 113 / Safari 16.4 / Firefox 91），更老的
 * 浏览器取消时什么都不触发——promise 挂在那里，无副作用，接受。
 */
export function pickFiles(): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    input.style.display = "none";
    const finish = (files: File[]) => {
      input.remove();
      resolve(files);
    };
    input.addEventListener("change", () => finish(Array.from(input.files ?? [])), { once: true });
    input.addEventListener("cancel", () => finish([]), { once: true });
    // Safari 要求 input 在文档里才响应 click()
    document.body.append(input);
    input.click();
  });
}

/**
 * 弹系统文件夹选择框。选中的每个 File 带 `webkitRelativePath`（相对被选中
 * 的那一层，含顶层文件夹名），上传时按这段路径在工作目录里重建树。
 *
 * 空文件夹浏览器给不出来——没有文件就没有 File，接受。
 */
export function pickFolder(): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    input.webkitdirectory = true;
    input.setAttribute("directory", "");
    input.style.display = "none";
    const finish = (files: File[]) => {
      input.remove();
      resolve(files);
    };
    input.addEventListener("change", () => finish(Array.from(input.files ?? [])), { once: true });
    input.addEventListener("cancel", () => finish([]), { once: true });
    document.body.append(input);
    input.click();
  });
}
