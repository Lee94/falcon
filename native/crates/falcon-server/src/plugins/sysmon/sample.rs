//! 一次采样的形状（本机与 Linux 远端共用），以及两边都要的过滤规则。纯函数。

/// 进程表按什么排：本机有瞬时 CPU（两次刷新之间的差），远端只有内存靠得住——`ps` 的 `%cpu`
/// 是进程一生的平均值，长跑的空闲守护进程会排在刚开始狂转的编译前面
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcSort {
    #[default]
    Cpu,
    Memory,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiskUse {
    pub mount: String,
    pub total: u64,
    pub available: u64,
}

impl DiskUse {
    /// 已用 = 总量 − 可用，不取卷自己的"已用"：APFS 一个容器里好几个卷共享空间，`/`（只读的系统卷）
    /// 自己只用了十来 GB，用户关心的是整个容器还剩多少——"总量 − 可用"与访达里的口径一致；
    /// ext4 的保留块也因此算进已用（df 的 Use% 同样如此）
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub name: String,
    /// 占一个核的百分比（top 的口径，多线程可以超过 100）；远端没有
    pub cpu: Option<f64>,
    pub mem: u64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub host: Option<String>,
    pub os: Option<String>,
    /// 秒
    pub uptime: Option<u64>,
    pub cores: usize,
    /// 整机 CPU 占用 0–100；第一次采样没有差值时为 None
    pub cpu: Option<f64>,
    pub load: Option<[f64; 3]>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// (下行, 上行) 字节 / 秒，所有计入的网卡合计
    pub net: Option<(f64, f64)>,
    pub disks: Vec<DiskUse>,
    pub procs: Vec<Proc>,
    pub procs_by: ProcSort,
}

/// 进程表留几行
pub const TOP_PROCS: usize = 8;

/// 计入网络合计的网卡。回环不算；VPN / 隧道（utun、tun、wg）上的流量同时也走了物理网卡，
/// 算进来就记了两遍；虚拟网桥 / 容器的 veth 同理（容器的流量出门还要过一次 eth0）；
/// macOS 的 awdl / llw（隔空投送的点对点链路）与 anpi / ap 这类常驻接口不是用户关心的流量
pub fn counts_toward_total(iface: &str) -> bool {
    const SKIP: &[&str] = &[
        "lo", "utun", "tun", "tap", "wg", "awdl", "llw", "bridge", "br-", "docker", "veth", "virbr", "vmnet", "gif",
        "stf", "anpi", "ap", "cni", "flannel", "cali", "tailscale", "zt",
    ];
    let name = iface.to_ascii_lowercase();
    !SKIP.iter().any(|p| name.starts_with(p))
}

/// 要不要列出这块盘。`fs` 本机是文件系统类型（apfs / ext4 / tmpfs），远端 `df` 给的是设备名
/// （/dev/sda1 / tmpfs / overlay）——两种写法里的伪文件系统一并挡掉
pub fn keep_disk(mount: &str, fs: &str, total: u64) -> bool {
    if total == 0 {
        return false;
    }
    // 根总是列：容器里的 / 就是 overlay，挡了就一块盘都没了
    if mount == "/" {
        return true;
    }
    const PSEUDO_FS: &[&str] = &[
        "tmpfs", "devtmpfs", "devfs", "shm", "udev", "none", "overlay", "squashfs", "autofs", "proc", "sysfs",
        "cgroup", "cgroup2", "efivarfs", "ramfs", "nullfs", "map", "fuse.lxcfs",
    ];
    let fs = fs.to_ascii_lowercase();
    if PSEUDO_FS.contains(&fs.as_str()) || fs.starts_with("/dev/loop") {
        return false;
    }
    // macOS：/System/Volumes/* 是同一个 APFS 容器里的系统卷（Data / VM / Preboot / Update…），
    // 与 / 同一份空间，列一个 / 就够了；/private/var/vm 同理
    const SKIP_MOUNTS: &[&str] = &["/proc", "/sys", "/dev", "/run", "/snap", "/var/lib/docker", "/System/Volumes/", "/private/var/vm", "/boot/efi"];
    if SKIP_MOUNTS.iter().any(|p| mount.starts_with(p)) {
        return false;
    }
    // 1 GiB 以下的多半是 EFI / 恢复分区
    total >= 1 << 30
}

/// 过滤 + 去重。总量与可用都相同的两块是同一份空间（APFS 同容器的卷、bind mount），只留先出现的；
/// 再把 `/` 排最前，其余按挂载点排
pub fn tidy_disks(disks: impl IntoIterator<Item = (DiskUse, String)>) -> Vec<DiskUse> {
    let mut out: Vec<DiskUse> = Vec::new();
    let mut all: Vec<(DiskUse, String)> = disks.into_iter().collect();
    all.sort_by(|a, b| (a.0.mount != "/").cmp(&(b.0.mount != "/")).then_with(|| a.0.mount.cmp(&b.0.mount)));
    for (d, fs) in all {
        if !keep_disk(&d.mount, &fs, d.total) {
            continue;
        }
        if out.iter().any(|o| o.total == d.total && o.available == d.available) {
            continue;
        }
        out.push(d);
    }
    out
}

/// 进程表：按 `by` 降序取前 [`TOP_PROCS`] 个
pub fn top_procs(mut procs: Vec<Proc>, by: ProcSort) -> Vec<Proc> {
    match by {
        ProcSort::Cpu => procs.sort_by(|a, b| {
            b.cpu.unwrap_or(0.0).total_cmp(&a.cpu.unwrap_or(0.0)).then_with(|| b.mem.cmp(&a.mem))
        }),
        ProcSort::Memory => procs.sort_by(|a, b| b.mem.cmp(&a.mem)),
    }
    procs.truncate(TOP_PROCS);
    procs
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn d(mount: &str, total: u64, available: u64) -> DiskUse {
        DiskUse { mount: mount.into(), total, available }
    }

    #[test]
    fn interfaces() {
        for keep in ["en0", "eth0", "wlan0", "enp3s0", "ens5", "Ethernet"] {
            assert!(counts_toward_total(keep), "{keep}");
        }
        for skip in ["lo", "lo0", "utun3", "awdl0", "llw0", "bridge100", "docker0", "veth12ab", "br-4f2", "wg0", "tailscale0", "anpi0", "ap1"] {
            assert!(!counts_toward_total(skip), "{skip}");
        }
    }

    #[test]
    fn disks_macos_apfs_container_collapses_to_root() {
        let disks = vec![
            (d("/System/Volumes/Data", 994 * GIB, 400 * GIB), "apfs".to_string()),
            (d("/", 994 * GIB, 400 * GIB), "apfs".into()),
            (d("/System/Volumes/VM", 994 * GIB, 400 * GIB), "apfs".into()),
            (d("/System/Volumes/iSCPreboot", GIB / 2, GIB / 4), "apfs".into()),
            (d("/Volumes/T7", 1000 * GIB, 600 * GIB), "exfat".into()),
            (d("/dev", 200 * 1024, 0), "devfs".into()),
        ];
        let out = tidy_disks(disks);
        assert_eq!(out.iter().map(|d| d.mount.as_str()).collect::<Vec<_>>(), vec!["/", "/Volumes/T7"]);
        assert_eq!(out[0].used(), 594 * GIB);
    }

    #[test]
    fn disks_linux_df_devices() {
        let disks = vec![
            (d("/", 50 * GIB, 20 * GIB), "overlay".to_string()),
            (d("/dev/shm", 64 * 1024 * 1024, 64 * 1024 * 1024), "shm".into()),
            (d("/run", 2 * GIB, 2 * GIB), "tmpfs".into()),
            (d("/snap/core/1", 2 * GIB, 0), "/dev/loop3".into()),
            (d("/boot/efi", 2 * GIB, GIB), "/dev/sda1".into()),
            (d("/home", 900 * GIB, 100 * GIB), "/dev/sda3".into()),
            (d("/etc/hosts", 50 * GIB, 20 * GIB), "/dev/vda1".into()),
        ];
        let out = tidy_disks(disks);
        // /etc/hosts 是容器里 bind 进来的文件，与 / 同一份空间，按总量 + 可用去重
        assert_eq!(out.iter().map(|d| d.mount.as_str()).collect::<Vec<_>>(), vec!["/", "/home"]);
    }

    #[test]
    fn top_processes() {
        let p = |name: &str, cpu: Option<f64>, mem: u64| Proc { name: name.into(), cpu, mem };
        let procs: Vec<Proc> = (0..12).map(|i| p(&format!("p{i}"), Some(i as f64), (12 - i) as u64)).collect();
        let by_cpu = top_procs(procs.clone(), ProcSort::Cpu);
        assert_eq!(by_cpu.len(), TOP_PROCS);
        assert_eq!(by_cpu[0].name, "p11");
        let by_mem = top_procs(procs, ProcSort::Memory);
        assert_eq!(by_mem[0].name, "p0");
    }
}
