//! docker CLI 输出 → 面板要用的形状。纯函数，零 I/O。
//!
//! 样本都是这台机器上 Docker 29.8（OrbStack）实测的输出。读法偏宽：字段缺了给缺省值，
//! 多出来的不管——老版本的 docker 少几个字段，不该让整个面板报错。

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::Value;

/// 一个容器（`container inspect` 的一项）
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Container {
    /// 64 位十六进制
    pub id: String,
    /// 去掉了 inspect 给的前导 `/`
    pub name: String,
    pub image: String,
    pub status: Status,
    pub exit_code: i64,
    pub oom_killed: bool,
    /// 守护进程记下的错误（起不来时的原因）
    pub error: String,
    /// `healthy` / `unhealthy` / `starting`；没配健康检查是 None
    pub health: Option<String>,
    /// unix 秒；docker 用 `0001-01-01T00:00:00Z` 表示"从没有过"，这里是 None
    pub created: Option<i64>,
    pub started: Option<i64>,
    pub finished: Option<i64>,
    pub restart_count: i64,
    pub restart_policy: String,
    pub restart_max: i64,
    pub project: Option<String>,
    pub service: Option<String>,
    pub ports: Vec<Port>,
    /// entrypoint + cmd，就是容器里跑的那条命令
    pub command: Vec<String>,
    pub mounts: Vec<Mount>,
    /// 网络名 → 容器在里面的 IP
    pub networks: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    Running,
    Paused,
    Restarting,
    Exited,
    #[default]
    Created,
    Removing,
    Dead,
}

impl Status {
    fn of(s: &str) -> Self {
        match s {
            "running" => Status::Running,
            "paused" => Status::Paused,
            "restarting" => Status::Restarting,
            "exited" => Status::Exited,
            "removing" => Status::Removing,
            "dead" => Status::Dead,
            _ => Status::Created,
        }
    }

    /// 停着的（可以删、可以 prune 掉的）
    pub fn stopped(self) -> bool {
        matches!(self, Status::Exited | Status::Created | Status::Dead)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    /// `22/tcp`
    pub container: String,
    /// 发布到宿主机上的地址；None = 只是 EXPOSE 了、没发布
    pub host: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// `bind` / `volume` / `tmpfs`
    pub kind: String,
    /// bind 是宿主机路径，volume 是卷名
    pub source: String,
    pub destination: String,
    pub rw: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawContainer {
    id: String,
    name: String,
    created: String,
    restart_count: i64,
    state: RawState,
    config: RawConfig,
    host_config: RawHostConfig,
    network_settings: RawNetworkSettings,
    mounts: Option<Vec<RawMount>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawState {
    status: String,
    #[serde(rename = "OOMKilled")]
    oom_killed: bool,
    exit_code: i64,
    error: String,
    started_at: String,
    finished_at: String,
    health: Option<RawHealth>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawHealth {
    status: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawConfig {
    image: String,
    labels: Option<BTreeMap<String, String>>,
    cmd: Option<Value>,
    entrypoint: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawHostConfig {
    restart_policy: Option<RawRestart>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawRestart {
    name: String,
    maximum_retry_count: i64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawNetworkSettings {
    ports: Option<BTreeMap<String, Option<Vec<RawBinding>>>>,
    networks: Option<BTreeMap<String, RawNetwork>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawBinding {
    host_ip: String,
    host_port: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawNetwork {
    #[serde(rename = "IPAddress")]
    ip_address: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawMount {
    #[serde(rename = "Type")]
    kind: String,
    name: String,
    source: String,
    destination: String,
    #[serde(rename = "RW")]
    rw: bool,
}

/// Cmd / Entrypoint：数组，老镜像里偶尔是一个字符串
fn words(v: Option<Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.into_iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s],
        _ => Vec::new(),
    }
}

impl From<RawContainer> for Container {
    fn from(r: RawContainer) -> Self {
        let labels = r.config.labels.unwrap_or_default();
        let label = |k: &str| labels.get(k).filter(|v| !v.is_empty()).cloned();
        let mut ports = Vec::new();
        for (container, bindings) in r.network_settings.ports.unwrap_or_default() {
            match bindings.filter(|b| !b.is_empty()) {
                None => ports.push(Port { container, host: None }),
                Some(bs) => {
                    // 同一个端口在 0.0.0.0 与 :: 上各发布一次（IPv4 / IPv6）：看着是重复，并成一条
                    let mut seen: Vec<(String, String)> = Vec::new();
                    for b in bs {
                        let ip = if b.host_ip == "::" || b.host_ip == "0.0.0.0" { String::new() } else { b.host_ip };
                        let pair = (ip, b.host_port);
                        if !seen.contains(&pair) {
                            seen.push(pair.clone());
                            ports.push(Port { container: container.clone(), host: Some(pair) });
                        }
                    }
                }
            }
        }
        // 发布了的在前，按宿主机端口的数值排（"8080" 排在 "443" 后面）
        ports.sort_by_key(|p| (p.host.is_none(), p.host.as_ref().and_then(|(_, port)| port.parse::<u32>().ok()), p.container.clone()));
        let restart = r.host_config.restart_policy.unwrap_or_default();
        let mut command = words(r.config.entrypoint);
        command.extend(words(r.config.cmd));
        Container {
            name: r.name.trim_start_matches('/').to_string(),
            image: r.config.image,
            status: Status::of(&r.state.status),
            exit_code: r.state.exit_code,
            oom_killed: r.state.oom_killed,
            error: r.state.error,
            health: r.state.health.map(|h| h.status).filter(|s| !s.is_empty() && s != "none"),
            created: parse_time(&r.created),
            started: parse_time(&r.state.started_at),
            finished: parse_time(&r.state.finished_at),
            restart_count: r.restart_count,
            restart_policy: restart.name,
            restart_max: restart.maximum_retry_count,
            project: label("com.docker.compose.project"),
            service: label("com.docker.compose.service"),
            ports,
            command,
            mounts: r
                .mounts
                .unwrap_or_default()
                .into_iter()
                .map(|m| Mount {
                    source: if m.kind == "volume" && !m.name.is_empty() { m.name } else { m.source },
                    kind: m.kind,
                    destination: m.destination,
                    rw: m.rw,
                })
                .collect(),
            networks: r
                .network_settings
                .networks
                .unwrap_or_default()
                .into_iter()
                .map(|(name, n)| (name, n.ip_address))
                .collect(),
            id: r.id,
        }
    }
}

/// `container inspect --format <模板>` 的输出：一行一个。读不了的行跳过（一个容器在两条命令之间被删了，
/// inspect 退非零但其余的照样打出来）
pub fn containers(stdout: &str) -> Vec<Container> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<RawContainer>(l).ok())
        .filter(|r| !r.id.is_empty())
        .map(Container::from)
        .collect()
}

/// `docker stats --no-stream` 的一行
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stats {
    /// 按单核计的百分比（docker 的口径：两核跑满是 200%）
    pub cpu: Option<f64>,
    /// 已用内存（字节），从 `MemUsage` 的前半截读
    pub mem: Option<f64>,
    /// `1.059MiB / 7.816GiB`，原样
    pub mem_text: String,
    pub mem_pct: Option<f64>,
    /// `6.75kB / 438B`（收 / 发），原样
    pub net: String,
    pub block: String,
    pub pids: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawStats {
    #[serde(rename = "ID")]
    id: String,
    container: String,
    #[serde(rename = "CPUPerc")]
    cpu_perc: String,
    mem_usage: String,
    mem_perc: String,
    #[serde(rename = "NetIO")]
    net_io: String,
    #[serde(rename = "BlockIO")]
    block_io: String,
    #[serde(rename = "PIDs")]
    pids: String,
}

fn percent(s: &str) -> Option<f64> {
    s.trim().trim_end_matches('%').trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// docker 打的大小（`1.059MiB`、`8.41MB`、`438B`、`7.816GiB`）→ 字节。`kB` 是 1000、`KiB` 是 1024
pub fn parse_size(s: &str) -> Option<f64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult = match unit.trim() {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(n * mult)
}

/// 按容器 id 存（`--no-trunc` 时是 64 位；老版本给 12 位前缀，查的时候按前缀对，见 [`stats_for`]）
pub fn stats(stdout: &str) -> HashMap<String, Stats> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<RawStats>(l.trim()).ok())
        .filter_map(|r| {
            let id = if r.id.is_empty() { r.container } else { r.id };
            (!id.is_empty()).then(|| {
                let mem = r.mem_usage.split('/').next().and_then(parse_size);
                (
                    id,
                    Stats {
                        cpu: percent(&r.cpu_perc),
                        mem,
                        mem_text: r.mem_usage.trim().to_string(),
                        mem_pct: percent(&r.mem_perc),
                        net: r.net_io.trim().to_string(),
                        block: r.block_io.trim().to_string(),
                        pids: r.pids.trim().to_string(),
                    },
                )
            })
        })
        .collect()
}

pub fn stats_for<'a>(all: &'a HashMap<String, Stats>, id: &str) -> Option<&'a Stats> {
    all.get(id).or_else(|| all.iter().find(|(k, _)| id.starts_with(k.as_str())).map(|(_, v)| v))
}

/// `docker info` 里面板顶上要的那几样
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Daemon {
    pub version: String,
    /// `OrbStack`、`Docker Desktop`、`Ubuntu 22.04.4 LTS`
    pub os: String,
    pub cpus: u32,
    pub mem_total: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawInfo {
    #[serde(rename = "ServerVersion")]
    server_version: String,
    #[serde(rename = "OperatingSystem")]
    operating_system: String,
    #[serde(rename = "NCPU")]
    ncpu: u32,
    #[serde(rename = "MemTotal")]
    mem_total: u64,
}

/// 守护进程连不上时 `docker info --format` 照样退 0、打出客户端那一半：没有版本号就当没拿到
pub fn daemon(stdout: &str) -> Option<Daemon> {
    let r: RawInfo = serde_json::from_str(stdout.trim()).ok()?;
    (!r.server_version.is_empty()).then(|| Daemon {
        version: r.server_version,
        os: r.operating_system,
        cpus: r.ncpu,
        mem_total: r.mem_total,
    })
}

/// `docker image ls` 的一行
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Image {
    /// 12 位短 id
    pub id: String,
    /// `<none>` 是悬空镜像（重新 build 之后被顶掉标签的老版本）
    pub repository: String,
    pub tag: String,
    /// docker 的写法（`1.62GB`），原样
    pub size: String,
    pub created: Option<i64>,
    /// 有几个容器（含停了的）在用它；老版本的 docker 给 `N/A`，这里是 None
    pub containers: Option<u32>,
}

impl Image {
    pub fn dangling(&self) -> bool {
        self.repository == "<none>" || self.repository.is_empty()
    }

    /// 显示与删除用的引用：有标签用 `repo:tag`（只删这一个标签，别的标签指着同一个镜像也不碍事），
    /// 悬空的只有 id
    pub fn reference(&self) -> String {
        if self.dangling() {
            self.id.clone()
        } else if self.tag.is_empty() || self.tag == "<none>" {
            self.repository.clone()
        } else {
            format!("{}:{}", self.repository, self.tag)
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawImage {
    #[serde(rename = "ID")]
    id: String,
    repository: String,
    tag: String,
    size: String,
    created_at: String,
    containers: String,
}

pub fn images(stdout: &str) -> Vec<Image> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<RawImage>(l.trim()).ok())
        .filter(|r| !r.id.is_empty())
        .map(|r| Image {
            id: r.id.trim_start_matches("sha256:").to_string(),
            repository: r.repository,
            tag: r.tag,
            size: r.size,
            created: parse_local_time(&r.created_at),
            containers: r.containers.parse().ok(),
        })
        .collect()
}

/// `docker system df` 的一行
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiskUse {
    /// `Images` / `Containers` / `Local Volumes` / `Build Cache`
    pub kind: String,
    pub count: String,
    pub active: String,
    pub size: String,
    /// `3.48GB (69%)`
    pub reclaimable: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawDf {
    #[serde(rename = "Type")]
    kind: String,
    total_count: String,
    active: String,
    size: String,
    reclaimable: String,
}

pub fn disk_use(stdout: &str) -> Vec<DiskUse> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<RawDf>(l.trim()).ok())
        .filter(|r| !r.kind.is_empty())
        .map(|r| DiskUse { kind: r.kind, count: r.total_count, active: r.active, size: r.size, reclaimable: r.reclaimable })
        .collect()
}

/// 公历日期 → 1970-01-01 起的天数（Howard Hinnant 的 days_from_civil）
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn num(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// `YYYY-MM-DD` 与 `HH:MM:SS` → unix 秒（不含时区）
fn civil(date: &str, time: &str) -> Option<i64> {
    let mut d = date.split('-');
    let (y, mo, da) = (num(d.next()?)?, num(d.next()?)?, num(d.next()?)?);
    let mut t = time.split(':');
    let (h, mi, s) = (num(t.next()?)?, num(t.next()?)?, num(t.next()?)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, da) * 86400 + h * 3600 + mi * 60 + s)
}

/// `+08:00` / `+0800` / `-05:00` → 秒
fn offset(s: &str) -> Option<i64> {
    let sign = match s.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = s[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 {
        return None;
    }
    Some(sign * (num(&digits[..2])? * 3600 + num(&digits[2..])? * 60))
}

/// inspect 的时间（RFC 3339，`2026-10-16T14:55:43.348407113Z`）→ unix 秒。docker 拿
/// `0001-01-01T00:00:00Z` 表示"没有"，那是 None
pub fn parse_time(s: &str) -> Option<i64> {
    let (date, rest) = s.trim().split_once('T')?;
    if date.starts_with("0001-") {
        return None;
    }
    let cut = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
    let (clock, zone) = rest.split_at(cut);
    let clock = clock.split('.').next()?;
    let base = civil(date, clock)?;
    let off = match zone {
        "" | "Z" => 0,
        z => offset(z)?,
    };
    Some(base - off)
}

/// `docker image ls` 的时间：`2026-10-20 16:13:46 +0800 CST`（守护进程那台机器的本地时间带偏移）
pub fn parse_local_time(s: &str) -> Option<i64> {
    let mut it = s.split_whitespace();
    let (date, clock, zone) = (it.next()?, it.next()?, it.next()?);
    Some(civil(date, clock)? - offset(zone)?)
}

/// 去掉终端控制序列（日志里的颜色、清屏）与别的控制字符，留下换行与制表
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI：参数与中间字节，直到 0x40–0x7E 的终止字节
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC / DCS / APC / PM：到 BEL 或 ESC \ 为止
                Some(']' | 'P' | '_' | '^') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // 其余两字节的转义（ESC c、ESC 7 …）
                _ => {}
            },
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// 时间戳排序用的键：秒之前原样（`2026-10-16T14:55:43`），小数补齐到 9 位。docker 的 RFC3339Nano 会
/// 去掉尾部的 0（`.12Z` 与 `.123Z`），直接按字符串比会把 .12 排到 .123 后面
fn ts_key(ts: &str) -> Option<String> {
    let ts = ts.strip_suffix('Z')?;
    let (base, frac) = ts.split_once('.').unwrap_or((ts, ""));
    if base.len() != 19 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{base}.{frac:0<9}"))
}

/// `docker logs -t` 的 stdout 与 stderr 按时间戳合成一份，去掉时间戳与控制序列，留最后 `max_lines` 行、
/// 最多 `max_chars` 个字符（从新的那头留）。没有时间戳的行（不该有，防着）跟在前一行后面
pub fn merge_logs(stdout: &str, stderr: &str, max_lines: usize, max_chars: usize) -> String {
    let mut lines: Vec<(String, usize, &str)> = Vec::new();
    for (stream, text) in [stdout, stderr].into_iter().enumerate() {
        let mut last = String::new();
        for raw in text.lines() {
            let (key, body) = match raw.split_once(' ').and_then(|(ts, body)| Some((ts_key(ts)?, body))) {
                Some((k, b)) => (k, b),
                None => (last.clone(), raw),
            };
            last = key.clone();
            lines.push((key, stream, body));
        }
    }
    // 稳定排序：同一时刻的行保持各自流里的先后，stdout 在前
    lines.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    let start = lines.len().saturating_sub(max_lines);
    let text = lines[start..].iter().map(|(_, _, body)| strip_ansi(body)).collect::<Vec<_>>().join("\n");
    let n = text.chars().count();
    if n <= max_chars {
        return text;
    }
    // 从新的那头留，切在行首（切点正好在行首就不用再丢；整段只有一行就硬切）
    let cut = text.char_indices().nth(n - max_chars).map_or(0, |(i, _)| i);
    let tail = &text[cut..];
    let rest = if text[..cut].ends_with('\n') { tail } else { tail.split_once('\n').map_or(tail, |(_, r)| r) };
    format!("…\n{rest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实测样本：OrbStack 上跑着的 sshd 测试容器，用 cli.rs 的模板 inspect 出来
    const INSPECT: &str = r#"{"Id":"b7f1499cdf573c4bc0d4168a8a19daf2fed81141e0c8f5bee12cac1d2cd56f62","Name":"/falcon-test-sshd","Created":"2026-10-16T14:55:43.065333524Z","RestartCount":0,"State":{"Status":"running","Running":true,"Paused":false,"Restarting":false,"OOMKilled":false,"Dead":false,"Pid":1234,"ExitCode":0,"Error":"","StartedAt":"2026-10-16T14:55:43.348407113Z","FinishedAt":"0001-01-01T00:00:00Z"},"Config":{"Image":"falcon-test-sshd","Labels":{},"Cmd":["/usr/sbin/sshd","-D","-e"],"Entrypoint":null,"WorkingDir":"/"},"HostConfig":{"RestartPolicy":{"MaximumRetryCount":0,"Name":"no"}},"NetworkSettings":{"Ports":{"22/tcp":[{"HostIp":"127.0.0.1","HostPort":"2299"}]},"Networks":{"bridge":{"Aliases":null,"Gateway":"192.168.215.1","IPAddress":"192.168.215.2","IPPrefixLen":24}}},"Mounts":[]}"#;

    /// compose 起的、退出过的、有健康检查与卷的：手拼的，形状照 Docker 的文档
    const INSPECT_COMPOSE: &str = r#"{"Id":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","Name":"/shop-db-1","Created":"2026-10-01T00:00:00Z","RestartCount":3,"State":{"Status":"exited","OOMKilled":true,"ExitCode":137,"Error":"","StartedAt":"2026-10-20T08:00:00.5+08:00","FinishedAt":"2026-10-21T00:00:00Z","Health":{"Status":"unhealthy","FailingStreak":3,"Log":[]}},"Config":{"Image":"postgres:16","Labels":{"com.docker.compose.project":"shop","com.docker.compose.service":"db"},"Cmd":"postgres","Entrypoint":["docker-entrypoint.sh"]},"HostConfig":{"RestartPolicy":{"Name":"on-failure","MaximumRetryCount":5}},"NetworkSettings":{"Ports":{"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"5432"},{"HostIp":"::","HostPort":"5432"}],"9187/tcp":null,"80/tcp":[{"HostIp":"0.0.0.0","HostPort":"8080"}]},"Networks":null},"Mounts":[{"Type":"volume","Name":"shop_pg","Source":"/var/lib/docker/volumes/shop_pg/_data","Destination":"/var/lib/postgresql/data","RW":true},{"Type":"bind","Source":"/srv/init.sql","Destination":"/docker-entrypoint-initdb.d/init.sql","RW":false}]}"#;

    #[test]
    fn inspect_sample() {
        let v = containers(&format!("{INSPECT}\nnot json\n\n{INSPECT_COMPOSE}\n"));
        assert_eq!(v.len(), 2, "读不了的行跳过");
        let c = &v[0];
        assert_eq!(c.name, "falcon-test-sshd");
        assert_eq!(c.status, Status::Running);
        assert_eq!(c.health, None);
        assert_eq!(c.started, Some(1_792_162_543));
        assert_eq!(c.finished, None, "0001-01-01 是没有");
        assert_eq!(c.command, vec!["/usr/sbin/sshd", "-D", "-e"]);
        assert_eq!(c.ports, vec![Port { container: "22/tcp".into(), host: Some(("127.0.0.1".into(), "2299".into())) }]);
        assert_eq!(c.networks, vec![("bridge".into(), "192.168.215.2".into())]);
        assert_eq!((c.project.as_deref(), c.restart_policy.as_str()), (None, "no"));

        let d = &v[1];
        assert_eq!((d.status, d.exit_code, d.oom_killed), (Status::Exited, 137, true));
        assert!(d.status.stopped());
        assert_eq!(d.health.as_deref(), Some("unhealthy"));
        assert_eq!((d.project.as_deref(), d.service.as_deref()), (Some("shop"), Some("db")));
        assert_eq!(d.command, vec!["docker-entrypoint.sh", "postgres"], "entrypoint 在前，cmd 是字符串也收");
        assert_eq!(d.started, parse_time("2026-10-20T00:00:00Z"), "带时区偏移的换成 UTC");
        assert_eq!((d.restart_policy.as_str(), d.restart_max, d.restart_count), ("on-failure", 5, 3));
        // 0.0.0.0 与 :: 并成一条；发布了的按端口数值排在前，没发布的殿后
        let ports: Vec<_> = d.ports.iter().map(|p| (p.container.as_str(), p.host.clone())).collect();
        assert_eq!(
            ports,
            vec![
                ("5432/tcp", Some((String::new(), "5432".to_string()))),
                ("80/tcp", Some((String::new(), "8080".to_string()))),
                ("9187/tcp", None),
            ]
        );
        assert_eq!(d.mounts[0].source, "shop_pg", "卷显示卷名");
        assert_eq!(d.mounts[1].source, "/srv/init.sql");
        assert!(!d.mounts[1].rw);
        assert!(d.networks.is_empty());
    }

    #[test]
    fn stats_sample() {
        let out = r#"{"BlockIO":"8.41MB / 0B","CPUPerc":"12.50%","Container":"b7f1499cdf573c4bc0d4168a8a19daf2fed81141e0c8f5bee12cac1d2cd56f62","ID":"b7f1499cdf573c4bc0d4168a8a19daf2fed81141e0c8f5bee12cac1d2cd56f62","MemPerc":"0.01%","MemUsage":"1.059MiB / 7.816GiB","Name":"falcon-test-sshd","NetIO":"6.75kB / 438B","PIDs":"1"}
{"CPUPerc":"--","Container":"abcdefabcdef","ID":"abcdefabcdef","MemUsage":"-- / --"}"#;
        let s = stats(out);
        let a = stats_for(&s, "b7f1499cdf573c4bc0d4168a8a19daf2fed81141e0c8f5bee12cac1d2cd56f62").unwrap();
        assert_eq!(a.cpu, Some(12.5));
        assert_eq!(a.mem, Some(1.059 * 1024.0 * 1024.0));
        assert_eq!((a.mem_text.as_str(), a.net.as_str(), a.pids.as_str()), ("1.059MiB / 7.816GiB", "6.75kB / 438B", "1"));
        // 12 位前缀也对得上；读不出数的是 None
        let b = stats_for(&s, "abcdefabcdef0000000000000000000000000000000000000000000000000000").unwrap();
        assert_eq!((b.cpu, b.mem), (None, None));
        assert!(stats_for(&s, "ffffffffffff").is_none());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("438B"), Some(438.0));
        assert_eq!(parse_size("6.75kB"), Some(6750.0));
        assert_eq!(parse_size("1.5GiB"), Some(1.5 * 1024.0 * 1024.0 * 1024.0));
        assert_eq!(parse_size(" 2MB "), Some(2e6));
        assert_eq!(parse_size("--"), None);
        assert_eq!(parse_size("12XB"), None);
    }

    #[test]
    fn info_images_df_samples() {
        let d = daemon(r#"{"ServerVersion":"29.8.2","OperatingSystem":"OrbStack","NCPU":10,"MemTotal":8392605696,"Name":"orbstack"}"#)
            .unwrap();
        assert_eq!((d.version.as_str(), d.os.as_str(), d.cpus, d.mem_total), ("29.8.2", "OrbStack", 10, 8392605696));
        assert_eq!(daemon(r#"{"ServerErrors":["Cannot connect"]}"#), None);

        let imgs = images(concat!(
            r#"{"Containers":"0","CreatedAt":"2026-10-20 16:13:46 +0800 CST","CreatedSince":"45 hours ago","Digest":"<none>","ID":"5aabe105d210","Repository":"vulncheck/exporter-rendering","SharedSize":"N/A","Size":"1.62GB","Tag":"final","UniqueSize":"N/A"}"#,
            "\n",
            r#"{"Containers":"N/A","CreatedAt":"2026-10-01 00:00:00 +0000 UTC","ID":"sha256:86a8aefa495f","Repository":"<none>","Size":"1.22GB","Tag":"<none>"}"#
        ));
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0].reference(), "vulncheck/exporter-rendering:final");
        assert_eq!(imgs[0].created, parse_time("2026-10-20T08:13:46Z"));
        assert_eq!(imgs[0].containers, Some(0));
        assert!(imgs[1].dangling());
        assert_eq!((imgs[1].reference().as_str(), imgs[1].containers), ("86a8aefa495f", None));

        let df = disk_use(concat!(
            r#"{"Active":"1","Reclaimable":"3.48GB (69%)","Size":"4.992GB","TotalCount":"11","Type":"Images"}"#,
            "\n",
            r#"{"Active":"0","Reclaimable":"9.536GB","Size":"11.14GB","TotalCount":"112","Type":"Build Cache"}"#
        ));
        assert_eq!(df[0], DiskUse { kind: "Images".into(), count: "11".into(), active: "1".into(), size: "4.992GB".into(), reclaimable: "3.48GB (69%)".into() });
        assert_eq!(df[1].kind, "Build Cache");
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-03-01T00:00:00.999999Z"), Some(951_868_800));
        assert_eq!(parse_time("1970-01-01T08:00:00+08:00"), Some(0));
        assert_eq!(parse_time("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_time("garbage"), None);
        assert_eq!(parse_local_time("1970-01-01 08:00:00 +0800 CST"), Some(0));
        assert_eq!(parse_local_time("1970-01-01 00:00:00 +0000 UTC"), Some(0));
        assert_eq!(parse_local_time("yesterday"), None);
    }

    #[test]
    fn ansi_is_stripped() {
        assert_eq!(strip_ansi("\u{1b}[32mINFO\u{1b}[0m ready\r"), "INFO ready");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}x\u{1b}]8;;http://a\u{1b}\\y"), "xy");
        assert_eq!(strip_ansi("a\tb\u{1b}cc\u{0}"), "a\tbc");
    }

    #[test]
    fn logs_merge_by_timestamp() {
        let out = "2026-10-16T14:55:43.461916567Z Server listening\n2026-10-16T14:55:43.5Z \u{1b}[1mbold\u{1b}[0m\n";
        let err = "2026-10-16T14:55:43.47Z warn: x\n2026-10-16T14:55:44Z later\ncontinuation\n";
        let merged = merge_logs(out, err, 100, 10_000);
        assert_eq!(merged, "Server listening\nwarn: x\nbold\nlater\ncontinuation");
        assert_eq!(merge_logs(out, err, 2, 10_000), "later\ncontinuation");
        let long = merge_logs(out, err, 100, 12);
        assert_eq!(long, "…\ncontinuation", "从新的那头留，切在行首");
        assert_eq!(merge_logs("", "", 10, 10), "");
    }
}
