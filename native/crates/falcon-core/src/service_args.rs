//! 已安装的本机服务的启动参数（launchd plist 的 `ProgramArguments`）。
//!
//! 原生客户端托管本机服务时要用到两次：
//! - **升级时原样带上**：`falcon service install` 不带参数就写默认配置（127.0.0.1:4923、默认数据
//!   目录），装过自定义端口 / 数据目录的人一升级，服务就换了个家，原来的会话全看不见了；
//! - **"本机"配置按它连**：服务跑在哪个端口，客户端就连哪个。
//!
//! 解析口径与服务端 `config.ts` 的 `parseArgs` 一致：只认空格分隔的 `--host` / `--port` /
//! `--data-dir`，后出现的覆盖先出现的；`ProgramArguments` 里的程序路径（SEA 是
//! `<dataDir>/bin/falcon`，源码运行是 node + 入口脚本）不是这三个开关，自然被跳过。

/// 服务端 `parseArgs` 的默认端口
pub const DEFAULT_PORT: u16 = 4923;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceArgs {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub data_dir: Option<String>,
}

/// 从 `ProgramArguments` 里挑出三个开关。端口不是合法数字时当没写（服务端那边 `Number()`
/// 得到 NaN、listen 会失败——那种服务本来就起不来，客户端退回默认端口去连也无妨）
pub fn parse_service_args(argv: &[String]) -> ServiceArgs {
    let mut out = ServiceArgs::default();
    let mut i = 0;
    while i < argv.len() {
        let value = argv.get(i + 1).cloned();
        match argv[i].as_str() {
            "--host" => {
                out.host = value;
                i += 1;
            }
            "--port" => {
                out.port = value.and_then(|v| v.parse().ok());
                i += 1;
            }
            "--data-dir" => {
                out.data_dir = value;
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

impl ServiceArgs {
    /// 重新 `service install` 时要带的参数：只带原来写了的，没写的仍走服务端默认值
    pub fn to_install_args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(h) = &self.host {
            out.extend(["--host".to_string(), h.clone()]);
        }
        if let Some(p) = self.port {
            out.extend(["--port".to_string(), p.to_string()]);
        }
        if let Some(d) = &self.data_dir {
            out.extend(["--data-dir".to_string(), d.clone()]);
        }
        out
    }

    /// 本机客户端连它用的基址。监听通配地址（`0.0.0.0` / `::`）或没写时连回环；
    /// IPv6 字面量加方括号
    pub fn local_url(&self) -> String {
        let port = self.port.unwrap_or(DEFAULT_PORT);
        let host = match self.host.as_deref().map(str::trim) {
            None | Some("") | Some("0.0.0.0") | Some("::") | Some("[::]") => "127.0.0.1".to_string(),
            Some(h) if h.contains(':') && !h.starts_with('[') => format!("[{h}]"),
            Some(h) => h.to_string(),
        };
        format!("http://{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sea_program_arguments() {
        let a = parse_service_args(&argv(&["/Users/u/.mojito/bin/falcon", "--port", "6789", "--data-dir", "/Users/u/.mojito"]));
        assert_eq!(a.port, Some(6789));
        assert_eq!(a.data_dir.as_deref(), Some("/Users/u/.mojito"));
        assert_eq!(a.host, None);
        assert_eq!(a.to_install_args(), argv(&["--port", "6789", "--data-dir", "/Users/u/.mojito"]));
        assert_eq!(a.local_url(), "http://127.0.0.1:6789");
    }

    #[test]
    fn node_program_arguments_and_defaults() {
        let a = parse_service_args(&argv(&["/opt/homebrew/bin/node", "/x/dist/index.js"]));
        assert_eq!(a, ServiceArgs::default());
        assert!(a.to_install_args().is_empty());
        assert_eq!(a.local_url(), "http://127.0.0.1:4923");
    }

    #[test]
    fn later_flag_wins_and_bad_port_ignored() {
        let a = parse_service_args(&argv(&["falcon", "--port", "1", "--port", "7000", "--host", "0.0.0.0"]));
        assert_eq!(a.port, Some(7000));
        assert_eq!(a.local_url(), "http://127.0.0.1:7000");
        let b = parse_service_args(&argv(&["falcon", "--port", "abc"]));
        assert_eq!(b.port, None);
        // 开关在末尾、没有值
        let c = parse_service_args(&argv(&["falcon", "--data-dir"]));
        assert_eq!(c.data_dir, None);
    }

    #[test]
    fn host_forms() {
        let url = |h: &str| ServiceArgs { host: Some(h.into()), port: Some(1), data_dir: None }.local_url();
        assert_eq!(url("::"), "http://127.0.0.1:1");
        assert_eq!(url("::1"), "http://[::1]:1");
        assert_eq!(url("192.168.1.2"), "http://192.168.1.2:1");
        assert_eq!(url("localhost"), "http://localhost:1");
    }
}
