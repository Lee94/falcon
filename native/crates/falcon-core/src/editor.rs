//! 「用 Zed / VS Code 打开」项目：把项目目录拼成编辑器的自定义协议链接，交给系统去开。
//!
//! 编辑器跑在**客户端这台机器**上，项目目录却在宿主机上，所以分两种（[`EditorTarget`]）：
//! - SSH 项目走编辑器自己的远程开发（Zed 的 SSH 远程、VS Code 的 Remote - SSH），用的是这台
//!   机器自己的 ssh 配置与密钥——falcon 存的密码 / 私钥在服务端，不往外交。user@host:port 就是
//!   项目上那份，是服务端眼里的地址，换一台客户端未必连得通，那时由编辑器自己报错；
//! - 本地项目的目录在服务端那台机器上：只有服务端就在这台机器上（基址是回环地址）时才打得开，
//!   否则不给入口——拿服务端的域名去 SSH 是瞎猜，反代 / 隧道后面的更是一定不通。
//!
//! 走自定义协议而不是 spawn `zed` / `code`：从 Finder 起的 App 拿不到登录 shell 的 PATH，
//! `code` 命令还得用户自己装；协议链接装了编辑器就认，浏览器版也一样能用。
//!
//! 链接格式（各自源码里读来的，文档只写了一半）：
//! - Zed：`zed://file<路径>`、`zed://ssh/<user>@<host>[:<port>]<路径>`（crates/zed/src/zed/open_listener.rs）。
//!   `zed://file` 后面整段 percent-decode 后原样当路径；ssh 那条按 URL 解析，用户名与路径各自解码；
//! - VS Code：`vscode://file<路径>`、`vscode://vscode-remote/ssh-remote+<authority><路径>`
//!   （src/vs/code/electron-main/app.ts 的 getWindowOpenableFromProtocolUrl：本机路径它自己 stat
//!   分文件 / 文件夹，远端的一律当文件夹开）。authority 照 Remote - SSH 扩展的
//!   `HostInfo.toAuthorityString`（扩展 0.128 的 out/extension.js）：带了用户或端口、有大写、
//!   含 `/\+` 时是 `{"hostName","user","port"}` 这段 JSON 的十六进制——URI 的 authority 会被转成
//!   小写，原样放就丢了大小写。
//!
//! 两家的链接都只认绝对路径（VS Code 不展开 `~`，Zed 的 `zed://file` 也不展开）。工作目录写成
//! `~/x` 或相对路径时 [`editor_url`] 给 `None`，界面先找服务端展开（`/api/fs/list` 回的 `path`）
//! 再拼——家目录在宿主机上，客户端不知道是哪儿。

use falcon_proto::{Project, ProjectType};

use crate::js::{encode_uri_component, js_trim};
use crate::multi_path::common_parent_dir;

/// 认得的编辑器
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Editor {
    Zed,
    VsCode,
}

impl Editor {
    /// 菜单里的顺序
    pub const ALL: [Editor; 2] = [Editor::Zed, Editor::VsCode];

    /// 产品名，菜单与提示里原样显示（不翻译）
    pub fn name(self) -> &'static str {
        match self {
            Editor::Zed => "Zed",
            Editor::VsCode => "VS Code",
        }
    }
}

/// 项目目录对客户端来说在哪
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorTarget {
    /// 与客户端同一台机器（本地项目，且服务端就在这台机器上）
    Local { dir: String },
    /// 经 SSH 够得着（SSH 项目）
    Ssh { user: String, host: String, port: u16, dir: String },
}

impl EditorTarget {
    pub fn dir(&self) -> &str {
        match self {
            EditorTarget::Local { dir } | EditorTarget::Ssh { dir, .. } => dir,
        }
    }

    /// 换一个目录：`~` 打头的由服务端展开以后
    pub fn with_dir(&self, dir: String) -> Self {
        match self {
            EditorTarget::Local { .. } => EditorTarget::Local { dir },
            EditorTarget::Ssh { user, host, port, .. } => {
                EditorTarget::Ssh { user: user.clone(), host: host.clone(), port: *port, dir }
            }
        }
    }
}

/// 项目在编辑器里开哪个目录、从哪儿够得着。`server_here`：服务端与客户端在同一台机器上
/// （连的基址是回环地址）。
///
/// 开的是工作目录；多仓库容器没填工作目录时开成员的公共父目录（编辑器里一眼看全所有成员），
/// 没有像样的公共父目录（不同盘符、只剩根）就不给。普通项目没有工作目录（未填目录的 SSH
/// 项目）也不给——开一个家目录没有意义。本地项目而服务端在别的机器上，同样不给
pub fn editor_target(project: &Project, server_here: bool) -> Option<EditorTarget> {
    let dir = project
        .working_dir
        .as_deref()
        .map(js_trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let multi = project.multi.as_ref()?;
            let dirs: Vec<&str> = multi.repos.iter().map(|m| m.dir.as_str()).collect();
            Some(common_parent_dir(&dirs)).filter(|d| !d.is_empty())
        })?;
    match project.project_type {
        ProjectType::Ssh => {
            let ssh = project.ssh.as_ref()?;
            Some(EditorTarget::Ssh { user: ssh.username.clone(), host: ssh.host.clone(), port: ssh.port, dir })
        }
        ProjectType::Local => server_here.then_some(EditorTarget::Local { dir }),
    }
}

/// 编辑器的链接。目录不是绝对路径（`~/x`、相对路径）时给 `None`，先找服务端展开
pub fn editor_url(editor: Editor, target: &EditorTarget) -> Option<String> {
    let path = url_path(target.dir())?;
    Some(match (editor, target) {
        (Editor::Zed, EditorTarget::Local { .. }) => {
            // `zed://file` 后面的整段就是路径：Windows 的 `C:/x` 前面不能多一个 `/`
            let path = if is_drive_url_path(&path) { &path[1..] } else { &path[..] };
            format!("zed://file{path}")
        }
        (Editor::Zed, EditorTarget::Ssh { user, host, port, .. }) => {
            let host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.clone() };
            let port = if *port == 22 { String::new() } else { format!(":{port}") };
            format!("zed://ssh/{}@{host}{port}{path}", encode_uri_component(user))
        }
        (Editor::VsCode, EditorTarget::Local { .. }) => format!("vscode://file{path}"),
        (Editor::VsCode, EditorTarget::Ssh { user, host, port, .. }) => {
            format!("vscode://vscode-remote/ssh-remote+{}{path}", vscode_ssh_authority(user, host, *port))
        }
    })
}

/// Remote - SSH 的 authority（`HostInfo.toAuthorityString`）。端口 22 不写：与在扩展里手敲
/// `user@host` 连过的是同一个 authority，「最近打开」里不会多出一条
fn vscode_ssh_authority(user: &str, host: &str, port: u16) -> String {
    // IPv6 字面量不带方括号，扩展那边按 isIPv6 认
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    let port = (port != 22).then_some(port);
    let plain = user.is_empty() && port.is_none() && host.to_lowercase() == host && !host.contains(['/', '\\', '+']);
    if plain {
        return host.to_string();
    }
    // 键序照扩展里 JSON.stringify 的写法（hostName → user → port）；它解析时不看键序，
    // 但同一个主机拼出同一串，VS Code 的「最近打开」才认得是同一处
    let mut json = format!("{{\"hostName\":{}", json_string(host));
    if !user.is_empty() {
        json.push_str(&format!(",\"user\":{}", json_string(user)));
    }
    if let Some(port) = port {
        json.push_str(&format!(",\"port\":{port}"));
    }
    json.push('}');
    json.bytes().map(|b| format!("{b:02x}")).collect()
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// 宿主机上的绝对路径 → 链接里的路径：`/` 打头、各段百分号编码（`/` 与盘符的 `:` 留着）、
/// 去掉末尾的分隔符（根除外）。Windows 路径（`C:\x`）的分隔符换成 `/`、前面补一个 `/`
/// （`/C:/x`，VS Code 远端 URI 的写法）；POSIX 路径里的 `\` 是文件名的一部分，照编码。
/// 不是绝对路径给 `None`
fn url_path(dir: &str) -> Option<String> {
    let dir = js_trim(dir);
    let normalized = if dir.starts_with('/') {
        let t = dir.trim_end_matches('/');
        if t.is_empty() { "/".to_string() } else { t.to_string() }
    } else if has_drive_prefix(dir) {
        let t = dir.replace('\\', "/");
        let t = t.trim_end_matches('/');
        // 只剩盘符（`C:`）时把根的 `/` 补回去
        if t.len() == 2 { format!("/{t}/") } else { format!("/{t}") }
    } else {
        return None;
    };
    Some(normalized.split('/').map(|seg| encode_uri_component(seg).replace("%3A", ":")).collect::<Vec<_>>().join("/"))
}

/// `/C:/…`：[`url_path`] 给 Windows 路径拼出来的样子
fn is_drive_url_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':'
}

/// `/^[A-Za-z]:[\\/]/`
fn has_drive_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(json: &str) -> Project {
        serde_json::from_str(json).unwrap()
    }

    fn ssh(dir: &str) -> EditorTarget {
        EditorTarget::Ssh { user: "fay".into(), host: "dev.box".into(), port: 22, dir: dir.into() }
    }

    fn local(dir: &str) -> EditorTarget {
        EditorTarget::Local { dir: dir.into() }
    }

    /// 十六进制解回来，断言写的是 JSON 而不是一串看不懂的数字
    fn unhex(s: &str) -> String {
        let bytes: Vec<u8> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn target_follows_project_kind_and_where_the_server_is() {
        let p = project(r#"{"id":"p","name":"a","type":"local","workingDir":" /Users/fay/code/a ","createdAt":0}"#);
        assert_eq!(editor_target(&p, true), Some(local("/Users/fay/code/a")));
        // 服务端在别的机器上：本地项目的目录不在这台机器上，不给
        assert_eq!(editor_target(&p, false), None);

        let p = project(
            r#"{"id":"p","name":"a","type":"ssh","workingDir":"/srv/app","createdAt":0,
                "ssh":{"host":"Dev.Box","port":2222,"username":"root","authMethod":"key","hasSecret":false}}"#,
        );
        let want = EditorTarget::Ssh { user: "root".into(), host: "Dev.Box".into(), port: 2222, dir: "/srv/app".into() };
        // SSH 项目与服务端在哪无关
        assert_eq!(editor_target(&p, false), Some(want.clone()));
        assert_eq!(editor_target(&p, true), Some(want));
    }

    #[test]
    fn target_needs_a_directory() {
        // 未填目录的 SSH 项目
        let p = project(
            r#"{"id":"p","name":"a","type":"ssh","workingDir":"  ","createdAt":0,
                "ssh":{"host":"h","port":22,"username":"u","authMethod":"key","hasSecret":false}}"#,
        );
        assert_eq!(editor_target(&p, true), None);
        // 多仓库容器没填工作目录：开成员的公共父目录
        let p = project(
            r#"{"id":"p","name":"a","type":"local","createdAt":0,
                "multi":{"repos":[{"dir":"/w/front"},{"dir":"/w/back/"}]}}"#,
        );
        assert_eq!(editor_target(&p, true), Some(local("/w")));
        // 公共父目录只剩根：不给
        let p = project(
            r#"{"id":"p","name":"a","type":"local","createdAt":0,
                "multi":{"repos":[{"dir":"/a/x"},{"dir":"/b/y"}]}}"#,
        );
        assert_eq!(editor_target(&p, true), None);
        // 填了工作目录的容器照工作目录开
        let p = project(
            r#"{"id":"p","name":"a","type":"local","workingDir":"/w/all","createdAt":0,
                "multi":{"repos":[{"dir":"/w/front"},{"dir":"/w/back"}]}}"#,
        );
        assert_eq!(editor_target(&p, true), Some(local("/w/all")));
    }

    #[test]
    fn zed_urls() {
        assert_eq!(editor_url(Editor::Zed, &local("/Users/fay/code/a/")).as_deref(), Some("zed://file/Users/fay/code/a"));
        assert_eq!(
            editor_url(Editor::Zed, &local("/Users/fay/我的 项目#1?")).as_deref(),
            Some("zed://file/Users/fay/%E6%88%91%E7%9A%84%20%E9%A1%B9%E7%9B%AE%231%3F")
        );
        // Windows：`zed://file` 后面紧跟盘符
        assert_eq!(editor_url(Editor::Zed, &local(r"C:\Users\fay\a\")).as_deref(), Some("zed://fileC:/Users/fay/a"));
        // 端口 22 不写
        assert_eq!(editor_url(Editor::Zed, &ssh("/home/fay/app")).as_deref(), Some("zed://ssh/fay@dev.box/home/fay/app"));
        let t = EditorTarget::Ssh { user: r"corp\fay".into(), host: "::1".into(), port: 2222, dir: "/".into() };
        assert_eq!(editor_url(Editor::Zed, &t).as_deref(), Some("zed://ssh/corp%5Cfay@[::1]:2222/"));
    }

    #[test]
    fn vscode_urls() {
        assert_eq!(editor_url(Editor::VsCode, &local("/Users/fay/a b")).as_deref(), Some("vscode://file/Users/fay/a%20b"));
        assert_eq!(editor_url(Editor::VsCode, &local(r"D:\work")).as_deref(), Some("vscode://file/D:/work"));
        assert_eq!(editor_url(Editor::VsCode, &local("D:/")).as_deref(), Some("vscode://file/D:/"));

        let url = editor_url(Editor::VsCode, &ssh("/home/fay/app")).unwrap();
        let rest = url.strip_prefix("vscode://vscode-remote/ssh-remote+").unwrap();
        let (hex, path) = rest.split_once('/').unwrap();
        assert_eq!(unhex(hex), r#"{"hostName":"dev.box","user":"fay"}"#);
        assert_eq!(path, "home/fay/app");

        let t = EditorTarget::Ssh { user: "Admin".into(), host: "[fe80::1]".into(), port: 2200, dir: r"C:\Users\Admin\src".into() };
        let url = editor_url(Editor::VsCode, &t).unwrap();
        let rest = url.strip_prefix("vscode://vscode-remote/ssh-remote+").unwrap();
        let (hex, path) = rest.split_once('/').unwrap();
        assert_eq!(unhex(hex), r#"{"hostName":"fe80::1","user":"Admin","port":2200}"#);
        assert_eq!(path, "C:/Users/Admin/src");
    }

    #[test]
    fn vscode_authority_plain_only_when_nothing_would_be_lost() {
        assert_eq!(vscode_ssh_authority("", "dev.box", 22), "dev.box");
        // 大写、端口、`+` 都要编码（authority 会被转小写；`+` 是 resolver 名的分隔符）
        assert_eq!(unhex(&vscode_ssh_authority("", "Dev.Box", 22)), r#"{"hostName":"Dev.Box"}"#);
        assert_eq!(unhex(&vscode_ssh_authority("", "dev", 2222)), r#"{"hostName":"dev","port":2222}"#);
        assert_eq!(unhex(&vscode_ssh_authority("", "a+b", 22)), r#"{"hostName":"a+b"}"#);
        assert_eq!(unhex(&vscode_ssh_authority("u\"q", "h", 22)), r#"{"hostName":"h","user":"u\"q"}"#);
    }

    #[test]
    fn relative_and_home_paths_wait_for_the_server() {
        for dir in ["~", "~/code", r"~\code", "code/app", "C:"] {
            assert_eq!(editor_url(Editor::Zed, &local(dir)), None, "{dir}");
            assert_eq!(editor_url(Editor::VsCode, &ssh(dir)), None, "{dir}");
        }
        // 服务端展开以后照常拼
        let t = ssh("~/code").with_dir("/home/fay/code".into());
        assert_eq!(editor_url(Editor::Zed, &t).as_deref(), Some("zed://ssh/fay@dev.box/home/fay/code"));
        assert_eq!(url_path("/"), Some("/".into()));
    }
}
