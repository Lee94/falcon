import { Fragment } from "react";
import { Check } from "lucide-react";
import { useApp } from "@/store.js";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/**
 * 溢出菜单。菜单内容由 useActions 统一产出（侧栏、总览、命令面板共用一份），
 * 打开方式是 store 里的一对坐标 —— 所以这里给 Radix 一个零尺寸的虚拟锚点，
 * 定位、键盘遍历、点外部关闭都交给 DropdownMenu。
 *
 * Esc 照例 preventDefault：多层浮层时只该关最上面那一层，由 App 统一分发。
 */
export function Menu() {
  const menu = useApp((s) => s.menu);
  const closeMenu = useApp((s) => s.closeMenu);

  return (
    <DropdownMenu
      open={menu !== null}
      onOpenChange={(next) => {
        if (!next) closeMenu();
      }}
      modal={false}
    >
      <DropdownMenuTrigger asChild>
        <span
          aria-hidden
          className="pointer-events-none fixed size-0"
          style={{ left: menu?.x ?? 0, top: menu?.y ?? 0 }}
        />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="end"
        side="bottom"
        sideOffset={4}
        className="min-w-54"
        onEscapeKeyDown={(e) => e.preventDefault()}
      >
        {menu?.items.map((item, i) => (
          <Fragment key={`${item.label}-${i}`}>
            {/* 危险项永远单独分组置底 */}
            {item.separated && i > 0 && <DropdownMenuSeparator />}
            <DropdownMenuItem
              variant={item.danger ? "destructive" : "default"}
              onSelect={() => item.onSelect()}
            >
              <span className="flex-1">{item.label}</span>
              {item.checked && <Check className="text-muted-foreground" />}
              {item.kbd && <DropdownMenuShortcut>{item.kbd}</DropdownMenuShortcut>}
            </DropdownMenuItem>
          </Fragment>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 从触发按钮算出菜单锚点：菜单右边缘对齐按钮右边缘，贴着按钮下沿展开 */
export function menuAnchor(e: { currentTarget: HTMLElement }): { x: number; y: number } {
  const r = e.currentTarget.getBoundingClientRect();
  return { x: Math.round(r.right), y: Math.round(r.bottom) };
}
