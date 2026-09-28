//! px0 在 falcon 源下的挂载前缀（ADR 0017）。对应 shared 的 `px0BasePath`。
//!
//! 原生客户端没有嵌 px0：菜单项把 `<服务地址><前缀>` 交给系统浏览器。浏览器没登录过
//! falcon 时，服务端会把它送去 `/?next=<前缀>` 登录，登录后回到 px0（web 的 lib/loginNext.ts）。

use crate::js::encode_uri_component;

/// `/px0/<项目 id>/`。服务端起 px0 时的 `-base-path`、反代路由、两个客户端开的地址都是它
pub fn px0_base_path(project_id: &str) -> String {
    format!("/px0/{}/", encode_uri_component(project_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_shared_px0_base_path() {
        assert_eq!(px0_base_path("d7697ab3-292d-4ef9-a8de-b10c4ebd962a"), "/px0/d7697ab3-292d-4ef9-a8de-b10c4ebd962a/");
        assert_eq!(px0_base_path("a/b c"), "/px0/a%2Fb%20c/");
    }
}
