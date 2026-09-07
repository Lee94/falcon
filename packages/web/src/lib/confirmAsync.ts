import { useApp, type ConfirmSpec } from "../store.js";

/**
 * 把 store 的回调式确认框包成 promise：确认 → true，取消 / 点外面关掉 → false。
 *
 * 顺序执行多个需要确认的步骤（逐个上传、逐个问"要覆盖吗"）时，回调式的
 * askConfirm 没法表达"等用户答完再继续"。取消没有回调，靠订阅 store 观察
 * confirm 从我们这份 spec 变成别的来判断。
 */
export function confirmAsync(spec: Omit<ConfirmSpec, "onConfirm">): Promise<boolean> {
  return new Promise((resolve) => {
    let confirmed = false;
    const full: ConfirmSpec = {
      ...spec,
      onConfirm: () => {
        confirmed = true;
        resolve(true);
      },
    };
    const unsubscribe = useApp.subscribe((state, prev) => {
      if (prev.confirm === full && state.confirm !== full) {
        unsubscribe();
        if (!confirmed) resolve(false);
      }
    });
    useApp.getState().askConfirm(full);
  });
}
