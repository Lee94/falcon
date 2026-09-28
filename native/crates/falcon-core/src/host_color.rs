//! 主机身份色。对应 web 的 `lib/hostColor.ts`。
//!
//! 由 user@host:port 稳定哈希到一个 hue，同一台机器在侧栏、标题栏、抽屉里永远是
//! 同一条色带。本地项目**不给**色条——"有颜色 = 在别人的机器上"这个信号必须
//! 独占，才有警示价值。
//!
//! 只哈希色相；深浅交给主题（web 是 CSS 变量 `--host-s` / `--host-l`，明暗各一套），
//! 于是同一台机器在浅色下是深色条、在深色下是浅色条，色相不变、认得出来。
//!
//! 与 web 的差别：web 直接吐 CSS 字符串（`hsl(210 var(--host-s) var(--host-l))`、
//! `var(--border)`、`transparent`），这里只给结构化的 [`HostBar`]，饱和度 / 亮度与
//! 半透明底色由 app 层按主题补上。

use falcon_proto::{Project, ProjectType, SshConfig, SshConfigInput, SshHost, SshHostInput, SystemInfo};

/// 避开 100–150（绿，与运行中冲突）和 0–20（红，与已丢失冲突）
const BANDS: [u32; 11] = [30, 50, 170, 190, 210, 230, 250, 270, 290, 310, 330];

/// FNV-1a 按 UTF-16 码元哈希（照 `charCodeAt` + `Math.imul` 的 32 位有符号算术），
/// 取 |h| 落到色带上。同一台机器在 web 与原生里必须是同一个颜色。
pub fn host_hue(key: &str) -> u32 {
    let mut h: i32 = 2_166_136_261_u32 as i32;
    for unit in key.encode_utf16() {
        h ^= i32::from(unit);
        h = h.wrapping_mul(16_777_619);
    }
    // 空串时 JS 的 h 还没被 `^=` 转成 int32，仍是 2166136261 这个正数
    let abs: i64 = if key.is_empty() { 2_166_136_261 } else { i64::from(h).abs() };
    BANDS[(abs % BANDS.len() as i64) as usize]
}

/// 有 user / host / port 的 SSH 端点（项目上的连接配置、已保存主机、两种表单输入）。
/// TS 那边是结构类型 `{ username; host; port }`。
pub trait SshEndpoint {
    fn username(&self) -> &str;
    fn host(&self) -> &str;
    fn port(&self) -> u16;
}

macro_rules! impl_endpoint {
    ($($t:ty),*) => {$(
        impl SshEndpoint for $t {
            fn username(&self) -> &str { &self.username }
            fn host(&self) -> &str { &self.host }
            fn port(&self) -> u16 { self.port }
        }
    )*};
}
impl_endpoint!(SshConfig, SshHost, SshHostInput, SshConfigInput);

/// user@host:port。主机列表和项目下拉共用，避免两处拼法漂移
pub fn ssh_conn(ssh: &impl SshEndpoint) -> String {
    format!("{}@{}:{}", ssh.username(), ssh.host(), ssh.port())
}

/// 项目的哈希键；没有 SSH 配置（本地项目）是空串
pub fn host_key(project: Option<&Project>) -> String {
    project.and_then(|p| p.ssh.as_ref()).map(ssh_conn).unwrap_or_default()
}

/// 身份色条
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostBar {
    /// 中性：web 的 `var(--border)`（要底色时是 `transparent`）
    Neutral,
    /// 这个色相 + 主题给的饱和度 / 亮度
    Hue(u32),
}

/// 项目的身份色：SSH 项目（且有连接配置）给色相，其余中性。
pub fn host_bar(project: Option<&Project>) -> HostBar {
    match project {
        Some(p) if p.project_type == ProjectType::Ssh && p.ssh.is_some() => HostBar::Hue(host_hue(&host_key(Some(p)))),
        _ => HostBar::Neutral,
    }
}

/// SSH 项目才有色条；本地项目返回 `None`，调用方据此不渲染色条。
/// 注意 SSH 项目缺连接配置时是 `Some(Neutral)`——与 web 的 `sshBar` 一致。
pub fn ssh_bar(project: Option<&Project>) -> Option<HostBar> {
    match project {
        Some(p) if p.project_type == ProjectType::Ssh => Some(host_bar(Some(p))),
        _ => None,
    }
}

/// 已保存主机的身份色，和引用它的项目同一条色带
pub fn host_bar_from_ssh(ssh: &impl SshEndpoint) -> HostBar {
    HostBar::Hue(host_hue(&ssh_conn(ssh)))
}

/// 连接串：SSH 为 user@host:port，本地为「本机 · 平台」
pub fn conn_label(project: Option<&Project>, system: Option<&SystemInfo>, local_word: &str) -> String {
    let Some(project) = project else { return String::new() };
    if project.project_type == ProjectType::Ssh
        && let Some(ssh) = &project.ssh {
            return ssh_conn(ssh);
        }
    match system.map(|s| s.platform.as_str()).filter(|p| !p.is_empty()) {
        Some(platform) => format!("{local_word} · {platform}"),
        None => local_word.to_string(),
    }
}

/// 侧栏 / 总览里那一列短标签：SSH 显示主机名，本地显示「本机」
pub fn host_label(project: Option<&Project>, local_word: &str) -> String {
    match project {
        None => String::new(),
        Some(p) => match (&p.project_type, &p.ssh) {
            (ProjectType::Ssh, Some(ssh)) => ssh.host.clone(),
            _ => local_word.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(json: &str) -> Project {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn hue_matches_web_for_known_keys() {
        // 期望值取自 web 的 hostHue（node 里跑出来的）
        assert_eq!(host_hue(""), 190);
        assert_eq!(host_hue("fay@172.16.25.134:22"), 50);
        assert_eq!(host_hue("root@10.0.0.2:22"), 30);
        assert_eq!(host_hue("用户@主机:2222"), 330);
        // 代理对按两个码元进哈希
        assert_eq!(host_hue("😀@x:1"), 210);
    }

    #[test]
    fn local_projects_get_no_bar() {
        let local = project(r#"{"id":"p","name":"l","type":"local","createdAt":0}"#);
        assert_eq!(host_bar(Some(&local)), HostBar::Neutral);
        assert_eq!(ssh_bar(Some(&local)), None);
        assert_eq!(host_key(Some(&local)), "");
        assert_eq!(host_label(Some(&local), "本机"), "本机");
        let sys: SystemInfo = serde_json::from_str(r#"{"platform":"darwin","localDurable":null,"version":"1"}"#).unwrap();
        assert_eq!(conn_label(Some(&local), Some(&sys), "本机"), "本机 · darwin");
        assert_eq!(conn_label(Some(&local), None, "本机"), "本机");
        assert_eq!(conn_label(None, None, "本机"), "");
    }

    #[test]
    fn ssh_projects_hash_their_endpoint() {
        let ssh = project(
            r#"{"id":"p","name":"box","type":"ssh","createdAt":0,
                "ssh":{"host":"172.16.25.134","port":22,"username":"fay","authMethod":"key","hasSecret":false}}"#,
        );
        assert_eq!(host_key(Some(&ssh)), "fay@172.16.25.134:22");
        assert_eq!(host_bar(Some(&ssh)), HostBar::Hue(50));
        assert_eq!(ssh_bar(Some(&ssh)), Some(HostBar::Hue(50)));
        assert_eq!(conn_label(Some(&ssh), None, "本机"), "fay@172.16.25.134:22");
        assert_eq!(host_label(Some(&ssh), "本机"), "172.16.25.134");
        // SSH 项目缺连接配置：有色条位置但是中性色
        let bare = project(r#"{"id":"p","name":"box","type":"ssh","createdAt":0}"#);
        assert_eq!(ssh_bar(Some(&bare)), Some(HostBar::Neutral));
    }
}
