//! falcon 后端本机的采样：sysinfo（macOS / Linux / Windows 一套接口）。
//!
//! sysinfo 的 CPU 占用（整机与每个进程）、网卡流量都是**两次刷新之间的差**，所以采样器是常驻的：
//! 攥着同一个 `System`，每次采样刷一遍、拿上一次到这一次的差。第一次没有"上一次"，先刷、
//! 等 sysinfo 要求的最小间隔再刷，面板打开的第一帧就有数。
//!
//! 实测（sysinfo 0.31，macOS）：进程的 CPU 要到它被看到之后的**第三次**刷新才有值，第二次恒为 0，
//! 与间隔长短无关（250ms / 1s 一样）。所以第一次采样先连刷两遍进程表、再等、再刷——不然打开面板的
//! 第一帧进程表全是 0%，按 CPU 排出来其实是按内存排的。之后新冒出来的进程头一次同样是 0，
//! 两秒后就有了，不另管。
//!
//! 调用都是阻塞的系统调用，外面包 `spawn_blocking`。

use std::time::{Duration, Instant};

use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System};

use super::sample::{DiskUse, Proc, ProcSort, Snapshot, counts_toward_total, tidy_disks, top_procs};

/// 挂载点不常变：隔这么久才重新列一次盘（其余时候只刷容量）
const DISK_RELIST: Duration = Duration::from_secs(30);
/// 网卡同理（插拔网线、开关 VPN）
const NET_RELIST: Duration = Duration::from_secs(30);

pub struct LocalSampler {
    sys: System,
    nets: Networks,
    disks: Disks,
    /// 上一次刷新的时刻：算网卡速率用
    last: Option<Instant>,
    disks_listed: Option<Instant>,
    nets_listed: Instant,
}

impl LocalSampler {
    pub fn new() -> Self {
        // Linux 上 sysinfo 默认给每个进程开着 /proc/<pid>/stat 不关（省下次 open），常驻的服务进程
        // 跟着就攥上几百个 fd；采样两秒一次，这点 open 不值得
        sysinfo::set_open_files_limit(0);
        LocalSampler {
            sys: System::new(),
            nets: Networks::new_with_refreshed_list(),
            disks: Disks::new(),
            last: None,
            disks_listed: None,
            nets_listed: Instant::now(),
        }
    }

    fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        // 进程只要 CPU 与内存：默认的 refresh_processes 还会去读每个进程的磁盘 IO 与可执行文件路径
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new().with_cpu().with_memory());
        if self.nets_listed.elapsed() >= NET_RELIST {
            self.nets.refresh_list();
            self.nets_listed = Instant::now();
        } else {
            self.nets.refresh();
        }
    }

    pub fn sample(&mut self) -> Snapshot {
        if self.last.is_none() {
            // 见模块注释：进程表要多刷一遍
            self.sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new().with_cpu().with_memory());
            self.refresh();
            self.last = Some(Instant::now());
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_millis(250)));
        }
        self.refresh();
        let now = Instant::now();
        let secs = self.last.map(|t| (now - t).as_secs_f64()).unwrap_or(0.0);
        self.last = Some(now);

        if self.disks_listed.is_none_or(|t| t.elapsed() >= DISK_RELIST) {
            self.disks.refresh_list();
            self.disks_listed = Some(now);
        } else {
            self.disks.refresh();
        }
        let disks = tidy_disks(self.disks.list().iter().map(|d| {
            (
                DiskUse {
                    mount: d.mount_point().to_string_lossy().into_owned(),
                    total: d.total_space(),
                    available: d.available_space(),
                },
                d.file_system().to_string_lossy().into_owned(),
            )
        }));

        let net = (secs > 0.0).then(|| {
            let (rx, tx) = self
                .nets
                .list()
                .iter()
                .filter(|(name, _)| counts_toward_total(name))
                .fold((0u64, 0u64), |(rx, tx), (_, n)| (rx + n.received(), tx + n.transmitted()));
            (rx as f64 / secs, tx as f64 / secs)
        });

        // Linux 上线程也在进程表里（thread_kind 有值），只看进程
        let procs = self
            .sys
            .processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
            .map(|p| Proc { name: p.name().to_string_lossy().into_owned(), cpu: Some(p.cpu_usage() as f64), mem: p.memory() })
            .collect();

        let total = self.sys.total_memory();
        // 已用 = 总量 − 可用（与 Linux 的 MemAvailable 同一口径，缓存不算占用）；个别平台上
        // sysinfo 拿不到"可用"（给 0），退回它自己的 used
        let available = self.sys.available_memory();
        let mem_used = if available > 0 && available <= total { total - available } else { self.sys.used_memory() };
        let load = System::load_average();
        Snapshot {
            host: System::host_name(),
            os: System::long_os_version().map(|v| os_name(&v)),
            uptime: Some(System::uptime()),
            cores: self.sys.cpus().len(),
            cpu: Some(self.sys.global_cpu_usage() as f64),
            // Windows 没有负载均值，sysinfo 给全零
            load: (!cfg!(windows)).then_some([load.one, load.five, load.fifteen]),
            mem_total: total,
            mem_used,
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            net,
            disks,
            procs: top_procs(procs, ProcSort::Cpu),
            procs_by: ProcSort::Cpu,
        }
    }
}

/// sysinfo 的系统名：去掉首尾空白（macOS 上带着一个尾巴空格），`MacOS` 改回苹果的写法
fn os_name(raw: &str) -> String {
    let s = raw.trim();
    match s.strip_prefix("MacOS") {
        Some(rest) => format!("macOS{rest}"),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_names() {
        assert_eq!(os_name("MacOS 27.0.1 "), "macOS 27.0.1");
        assert_eq!(os_name("Linux 22.04 Ubuntu"), "Linux 22.04 Ubuntu");
    }

    /// 真机冒烟：在跑测试的这台机器上采一次，形状说得过去
    #[test]
    fn samples_this_machine() {
        let mut s = LocalSampler::new();
        let snap = s.sample();
        assert!(snap.cores > 0);
        assert!(snap.mem_total > 0 && snap.mem_used <= snap.mem_total);
        let cpu = snap.cpu.unwrap();
        assert!((0.0..=100.0).contains(&cpu), "{cpu}");
        assert!(snap.net.is_some());
        assert!(snap.disks.iter().any(|d| d.mount == "/" || d.mount.ends_with(":\\")), "{:?}", snap.disks);
        assert!(!snap.procs.is_empty());
        // 第一帧的进程表就有 CPU（模块注释里那个"第三次刷新"）：总有个进程在动——至少是测试自己
        assert!(snap.procs.iter().any(|p| p.cpu.unwrap_or(0.0) > 0.0), "{:?}", snap.procs);
        // 第二次直接出差值，不再睡
        let started = Instant::now();
        s.sample();
        assert!(started.elapsed() < Duration::from_millis(200) + Duration::from_secs(2));
    }
}
