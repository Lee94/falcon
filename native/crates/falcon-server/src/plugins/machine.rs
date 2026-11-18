//! 内置插件共用的「看哪台机器」：系统监控与 Docker 的面板顶上都有一个「机器」选项（面板参数
//! [`PARAM`]）。`auto` 跟着焦点项目走（SSH 项目看它的远端、其余看本机），`local` 是 falcon 后端本机，
//! `host:<id>` 是一台已保存的 SSH Host。
//!
//! 远端的命令走项目自己那条链路或主机那条（与「浏览远端目录」共用，终端会话多半已经连着），不另开连接；
//! 链路状态是 `Rc` 的，所以要进会话引擎去拿（[`remote_exec`]）。命令行由调用方按宿主机的种类拼
//! （POSIX / Windows 的引号规则不同，`host::build_command_line`）。

use std::collections::BTreeMap;
use std::time::Duration;

use falcon_proto::{PluginBlock, PluginOption};

use super::{PluginError, resolve_project};
use crate::api::AppState;
use crate::db::{ProjectRow, SshHostRow};
use crate::engine::EngineHandle;
use crate::exec::{Exec as _, ExecResult};
use crate::sessions::ssh_host::HostFacts;

/// 面板参数名
pub const PARAM: &str = "machine";
pub const LOCAL_LABEL: &str = "本机";

/// 面板顶上的机器选择
#[derive(Debug, Clone, PartialEq)]
pub struct MachineChoice {
    /// 当前选中的值（`auto` / `local` / `host:<id>`）
    pub value: String,
    pub options: Vec<PluginOption>,
    /// 实际在看的那台的名字（"本机"、主机名、`user@host`）
    pub label: String,
}

impl MachineChoice {
    pub fn select(&self) -> PluginBlock {
        PluginBlock::Select {
            param: PARAM.into(),
            label: Some("机器".into()),
            value: Some(self.value.clone()),
            options: self.options.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Target {
    Local,
    /// SSH 项目：走项目自己那条链路
    Project(ProjectRow),
    Host(SshHostRow),
}

pub struct Resolved {
    /// 按机器分的状态（走势、缓存）用它当键：同一台机器从"跟随项目"与"选主机"两条路看到的是同一份
    pub key: String,
    pub target: Target,
    pub choice: MachineChoice,
}

#[derive(Debug)]
pub struct Unresolved {
    /// 即使焦点项目连不上，面板也要保留机器选择，让用户能切到另一台。
    pub choice: MachineChoice,
    pub error: PluginError,
}

pub async fn resolve(
    state: &AppState,
    params: &BTreeMap<String, String>,
    project_id: Option<&str>,
) -> Result<Resolved, Unresolved> {
    let db = &state.db;
    let value = params.get(PARAM).cloned().unwrap_or_else(|| "auto".into());
    let mut options = vec![
        PluginOption { value: "auto".into(), label: "跟随当前项目".into() },
        PluginOption { value: "local".into(), label: LOCAL_LABEL.into() },
    ];
    options.extend(db.list_hosts().into_iter().map(|h| PluginOption { value: format!("host:{}", h.id), label: h.name }));
    let local = |value, options| Resolved {
        key: "local".into(),
        target: Target::Local,
        choice: MachineChoice { value, options, label: LOCAL_LABEL.into() },
    };
    if value == "local" {
        return Ok(local(value, options));
    }
    if let Some(host) = value.strip_prefix("host:").and_then(|id| db.get_host(id)) {
        return Ok(Resolved {
            key: format!("host:{}", host.id),
            choice: MachineChoice { value, options, label: host.name.clone() },
            target: Target::Host(host),
        });
    }
    // auto（以及选过的主机后来被删了）：跟着焦点项目。远端项目解析失败不能退回本机，
    // 否则点 Docker 的启停或清理可能会作用在另一台机器上。
    let value = "auto".to_string();
    let mut choice = MachineChoice { value, options, label: "跟随当前项目".into() };
    let project = match resolve_project(state, project_id).await {
        Ok(project) => project.filter(|p| p.project_type == "ssh"),
        Err(error) => return Err(Unresolved { choice, error }),
    };
    let Some(project) = project else { return Ok(local(choice.value, choice.options)) };
    let bound = project.host_id.as_deref().and_then(|id| db.get_host(id));
    // 节点项目走网关已经用来验证身份的主机链路，无须再建一条按项目 id 索引的 SSH 连接。
    if project.id.starts_with("node:") {
        let Some(host) = bound.filter(|h| {
            project.ssh_host.as_deref() == Some(h.host.as_str())
                && project.ssh_port == Some(h.port)
                && project.ssh_username.as_deref() == Some(h.username.as_str())
        }) else {
            let error = PluginError::Failed("远端主机配置已改变，请重新选择机器".into());
            return Err(Unresolved { choice, error });
        };
        choice.label = host.name.clone();
        return Ok(Resolved { key: format!("host:{}", host.id), target: Target::Host(host), choice });
    }
    let conn = format!(
        "{}@{}",
        project.ssh_username.as_deref().unwrap_or(""),
        project.ssh_host.as_deref().unwrap_or("")
    );
    let (key, label) = match bound {
        Some(h) => (format!("host:{}", h.id), h.name),
        None => (format!("ssh:{conn}:{}", project.ssh_port.unwrap_or(22)), conn),
    };
    choice.label = label;
    Ok(Resolved { key, target: Target::Project(project), choice })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteError {
    /// 这台远端干不了这件事：带上它是什么（Windows / Darwin …），面板上如实说
    Unsupported(String),
    Failed(String),
}

/// 在远端跑一条命令。`build` 拿到宿主机的探测结果（POSIX / Windows、uname）拼出命令行，或说不支持。
/// 非零退出码照执行器的铁律是正常返回值，由调用方看 `code`；`Err` 只有链路故障、超时与不支持
pub async fn remote_exec<B>(engine: &EngineHandle, target: &Target, timeout: Duration, build: B) -> Result<ExecResult, RemoteError>
where
    B: FnOnce(&HostFacts) -> Result<String, RemoteError> + Send + 'static,
{
    let (project, host) = match target {
        Target::Project(row) => (Some(row.clone()), None),
        Target::Host(row) => (None, Some(row.clone())),
        Target::Local => return Err(RemoteError::Failed("本机不走远端链路".into())),
    };
    let res = engine
        .call(move |engine| async move {
            let link = match (project, host) {
                (Some(row), _) => engine.sessions.get_link(&row),
                (_, Some(host)) => engine.sessions.get_host_link(&host),
                _ => unreachable!(),
            };
            let run = async {
                let facts = link.host_facts().await.map_err(|e| {
                    RemoteError::Failed(match e.detail {
                        Some(d) => format!("{}（{d}）", e.message),
                        None => e.message,
                    })
                })?;
                let cmd = build(&facts)?;
                link.exec(&cmd, None).await.map_err(|e| RemoteError::Failed(format!("{e:#}")))
            };
            tokio::time::timeout(timeout, run).await.unwrap_or_else(|_| Err(RemoteError::Failed("超时".into())))
        })
        .await;
    res.unwrap_or_else(|e| Err(RemoteError::Failed(e.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(machine: Option<&str>) -> BTreeMap<String, String> {
        machine.map(|m| BTreeMap::from([(PARAM.to_string(), m.to_string())])).unwrap_or_default()
    }

    #[tokio::test]
    async fn resolution() {
        let app = crate::api::test_support::TestApp::new();
        let offline = crate::nodes::resource_id("offline-node", "p");
        // 没选机器、没有焦点项目时看本机。显式选机器与焦点节点是否在线无关。
        let r = resolve(&app.state, &params(None), None).await.unwrap();
        assert_eq!((r.key.as_str(), r.choice.value.as_str(), r.choice.label.as_str()), ("local", "auto", "本机"));
        assert!(matches!(r.target, Target::Local));
        assert_eq!(resolve(&app.state, &params(Some("local")), Some(&offline)).await.unwrap().choice.value, "local");
        assert_eq!(resolve(&app.state, &params(Some("host:gone")), None).await.unwrap().choice.value, "auto");
        assert_eq!(
            r.choice.options.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(),
            vec!["auto", "local"],
            "没有保存的主机时只有两项"
        );
        let PluginBlock::Select { param, value, .. } = r.choice.select() else { panic!() };
        assert_eq!((param.as_str(), value.as_deref()), (PARAM, Some("auto")));
    }
}
