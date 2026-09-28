//! 终端深浅线索：`packages/shared/src/termEnv.ts` 里的类型部分。
//!
//! termEnv.ts 其余都是服务端的运行时逻辑（写 PTY 环境、代答 OSC 10/11/12 的
//! `OscColorGate`），这里不镜像：原生客户端只负责把深浅与底字色报上去（WS 的
//! `appearance`、建会话的请求体），**自己不答颜色查询**——多个 Viewer 各答一次
//! 就是往 zellij 里敲垃圾（设计文档 §3.2）。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

wire_enum! {
    /// 终端配色的深浅，不是界面主题——用户可以浅色 UI + Solarized Dark。
    ///
    /// 服务端据此写 COLORFGBG / GROK_APPEARANCE：grok / vim / less 看不到画在客户端
    /// 上的主题，只看得到 PTY 环境，或启动时问一声 OSC 11。
    pub enum TermAppearance {
        Light => "light",
        Dark => "dark",
    }
}

/// 深浅 + 底字色，服务端代答 OSC 10/11/12 用。
///
/// 服务端入口（`sanitizeColorHint`）对非法值**整项丢掉**，不当成 4xx——深浅缺失
/// 只是退回不注入。所以颜色写错了不会报错，只会悄悄不生效。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct OscColorHint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<TermAppearance>,
    /// `#rgb` / `#rrggbb`（`#rrggbbaa` 也收，忽略 alpha）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn appearance_literals() {
        assert_eq!(roundtrip::<TermAppearance>(r#""light""#), TermAppearance::Light);
        assert_eq!(roundtrip::<TermAppearance>(r#""dark""#), TermAppearance::Dark);
        assert!(serde_json::from_str::<TermAppearance>(r#""auto""#).is_err());
    }

    #[test]
    fn color_hint_roundtrip() {
        let full = roundtrip::<OscColorHint>(
            r##"{"appearance":"dark","background":"#0a0a0a","foreground":"#fafafa"}"##,
        );
        assert_eq!(full.appearance, Some(TermAppearance::Dark));
        let empty = roundtrip::<OscColorHint>("{}");
        assert_eq!(empty, OscColorHint::default());
    }
}
