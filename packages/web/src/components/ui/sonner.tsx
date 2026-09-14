import {
  CircleCheckIcon,
  InfoIcon,
  Loader2Icon,
  OctagonXIcon,
  TriangleAlertIcon,
} from "lucide-react"
import { Toaster as Sonner, type ToasterProps } from "sonner"

import { useApp } from "@/store.js"

// 不接 next-themes：明暗已经在 store 里了（按当前主题的实际深浅，不按明暗模式），再引一层 provider 只是重复一份状态。
// description 用 pre-line —— 残留路径这类 body 是多行的，折行了才读得出来。
const Toaster = ({ ...props }: ToasterProps) => {
  const theme = useApp((s) => s.activeTheme.appearance)

  return (
    <Sonner
      theme={theme}
      className="toaster group"
      position="bottom-right"
      icons={{
        success: <CircleCheckIcon className="size-4" />,
        info: <InfoIcon className="size-4" />,
        warning: <TriangleAlertIcon className="size-4" />,
        error: <OctagonXIcon className="size-4" />,
        loading: <Loader2Icon className="size-4 animate-spin" />,
      }}
      toastOptions={{
        classNames: {
          description: "whitespace-pre-line",
          toast: "backdrop-blur-xl",
        },
      }}
      style={
        {
          // 半透明 + 背板模糊：toast 压在终端画面上，糊一层才不挡着底下在读的东西
          "--normal-bg": "color-mix(in oklab, var(--popover) 82%, transparent)",
          "--normal-text": "var(--popover-foreground)",
          "--normal-border": "var(--border)",
          "--border-radius": "var(--radius)",
        } as React.CSSProperties
      }
      {...props}
    />
  )
}

export { Toaster }
