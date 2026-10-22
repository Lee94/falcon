//! Linux 远端主机的采样：一条 POSIX sh 脚本把 /proc 里要的几个文件、`df`、`ps` 一次打回来，
//! 解析与算差值都在这边（纯函数，拿真实样本做单测）。
//!
//! 为什么不在远端跑 sysinfo：远端上唯一有的 falcon 二进制是会话守护进程（ADR 0020），给它加一个
//! 采样子命令就得把 sysinfo 编进 5 个目标的交叉编译里（apple-darwin 那两个要链系统框架，zig 交叉
//! 编不顺），还得为此推新的守护进程。/proc 的格式几十年没变过，一条脚本就够了。
//! macOS / Windows 远端暂不支持（macOS 没有能在 shell 里拿到的累计 CPU 计数，`top -l 2` 一次要等
//! 一秒），面板上如实说。
//!
//! 脚本只用 busybox 也有的东西（alpine 容器里实测）：`head` / `grep -c` / `cat` / `sed -n` /
//! `df -kP` / `ps`。busybox 的 `ps` 不认 `-e` 与 `rss=` 那套写法，所以退一步再试一种，解析时
//! 认不出的行（表头）跳过。

use super::sample::{DiskUse, Proc, ProcSort, Snapshot, counts_toward_total, tidy_disks, top_procs};

/// 远端执行的脚本，经 `sh -c` 跑（不依赖远端登录 shell 是什么——fish 也照样）。
/// 每段前面一行 `@@<名字>`，解析按段切
pub const PROBE: &str = r#"LC_ALL=C; export LC_ALL
echo @@stat; head -n 1 /proc/stat; grep -c '^cpu[0-9]' /proc/stat
echo @@meminfo; cat /proc/meminfo
echo @@loadavg; cat /proc/loadavg
echo @@uptime; cat /proc/uptime
echo @@net; cat /proc/net/dev
echo @@df; df -kP 2>/dev/null
echo @@os; sed -n 's/^PRETTY_NAME=//p' /etc/os-release 2>/dev/null
echo @@host; uname -n
echo @@ps; ps -eo rss=,comm= 2>/dev/null || ps -o rss,comm 2>/dev/null
"#;

/// 一次探测读到的累计计数（差值要两次之间算）
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Counters {
    /// /proc/stat 第一行：(总 jiffies, 空闲 jiffies)，空闲含 iowait
    pub cpu: Option<(u64, u64)>,
    pub cores: usize,
    pub mem_total: u64,
    pub mem_available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    pub load: Option<[f64; 3]>,
    pub uptime: Option<u64>,
    /// 计入的网卡的累计 (收, 发) 字节
    pub net: Option<(u64, u64)>,
    pub disks: Vec<DiskUse>,
    pub os: Option<String>,
    pub host: Option<String>,
    pub procs: Vec<Proc>,
}

fn sections(stdout: &str) -> Vec<(&str, Vec<&str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in stdout.lines() {
        if let Some(name) = line.strip_prefix("@@") {
            out.push((name.trim(), Vec::new()));
        } else if let Some((_, lines)) = out.last_mut() {
            lines.push(line);
        }
    }
    out
}

/// `/proc/stat` 的 `cpu  user nice system idle iowait irq softirq steal guest guest_nice`。
/// 总量取前 8 项：guest 已经算在 user 里了，再加一遍就重复了
fn parse_cpu(line: &str) -> Option<(u64, u64)> {
    let mut f = line.split_whitespace();
    if f.next()? != "cpu" {
        return None;
    }
    let v: Vec<u64> = f.take(8).map(|x| x.parse().ok()).collect::<Option<_>>()?;
    if v.len() < 4 {
        return None;
    }
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    Some((v.iter().sum(), idle))
}

/// `MemTotal:  16314584 kB` → 字节
fn meminfo(lines: &[&str], key: &str) -> Option<u64> {
    lines.iter().find_map(|l| {
        let rest = l.strip_prefix(key)?.strip_prefix(':')?;
        let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
        Some(kb * 1024)
    })
}

/// `/proc/net/dev`：两行表头之后 `  eth0: <收字节> … <发字节>（第 9 列）…`
fn parse_net(lines: &[&str]) -> Option<(u64, u64)> {
    let mut total: Option<(u64, u64)> = None;
    for l in lines {
        let Some((name, rest)) = l.split_once(':') else { continue };
        let name = name.trim();
        let f: Vec<&str> = rest.split_whitespace().collect();
        let (Some(rx), Some(tx)) = (f.first().and_then(|v| v.parse::<u64>().ok()), f.get(8).and_then(|v| v.parse::<u64>().ok()))
        else {
            continue;
        };
        let t = total.get_or_insert((0, 0));
        if counts_toward_total(name) {
            t.0 += rx;
            t.1 += tx;
        }
    }
    total
}

/// `df -kP`：`设备 1024-blocks 已用 可用 容量% 挂载点`（挂载点可能带空格，取第 6 列往后）
fn parse_df(lines: &[&str]) -> Vec<DiskUse> {
    tidy_disks(lines.iter().filter_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 6 {
            return None;
        }
        let total: u64 = f[1].parse().ok()?;
        let available: u64 = f[3].parse().ok()?;
        Some((DiskUse { mount: f[5..].join(" "), total: total * 1024, available: available * 1024 }, f[0].to_string()))
    }))
}

/// `ps` 的 RSS：procps 给 KiB 的整数；busybox 大了会写成 `12m` / `1.2g`
fn parse_rss(s: &str) -> Option<u64> {
    let (num, mul) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1024.0),
        'm' | 'M' => (&s[..s.len() - 1], 1024.0 * 1024.0),
        'g' | 'G' => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (s, 1024.0),
    };
    let v: f64 = num.parse().ok()?;
    (v.is_finite() && v >= 0.0).then(|| (v * mul) as u64)
}

fn parse_ps(lines: &[&str]) -> Vec<Proc> {
    lines
        .iter()
        .filter_map(|l| {
            let l = l.trim();
            let (rss, name) = l.split_once(char::is_whitespace)?;
            let mem = parse_rss(rss)?;
            let name = name.trim();
            // 内核线程（[kworker/0:1]）的 RSS 是 0，本来也排不上
            (!name.is_empty() && mem > 0).then(|| Proc { name: name.to_string(), cpu: None, mem })
        })
        .collect()
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').trim_matches('\'').to_string()
}

/// 解析探测输出。读不到 /proc/stat 与 /proc/meminfo 就当失败（不是 Linux，或者被 chroot 掉了）
pub fn parse_probe(stdout: &str) -> Result<Counters, String> {
    let mut c = Counters::default();
    for (name, lines) in sections(stdout) {
        match name {
            "stat" => {
                c.cpu = lines.first().and_then(|l| parse_cpu(l));
                c.cores = lines.get(1).and_then(|l| l.trim().parse().ok()).unwrap_or(0);
            }
            "meminfo" => {
                c.mem_total = meminfo(&lines, "MemTotal").unwrap_or(0);
                // 3.14 之前的内核没有 MemAvailable：退回 Free + Buffers + Cached
                c.mem_available = meminfo(&lines, "MemAvailable").unwrap_or_else(|| {
                    ["MemFree", "Buffers", "Cached"].iter().filter_map(|k| meminfo(&lines, k)).sum()
                });
                c.swap_total = meminfo(&lines, "SwapTotal").unwrap_or(0);
                c.swap_free = meminfo(&lines, "SwapFree").unwrap_or(0);
            }
            "loadavg" => {
                let v: Vec<f64> = lines.first().map(|l| l.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
                c.load = (v.len() == 3).then(|| [v[0], v[1], v[2]]);
            }
            "uptime" => {
                c.uptime = lines.first().and_then(|l| l.split_whitespace().next()).and_then(|x| x.parse::<f64>().ok()).map(|s| s as u64);
            }
            "net" => c.net = parse_net(&lines),
            "df" => c.disks = parse_df(&lines),
            "os" => c.os = lines.first().map(|l| unquote(l)).filter(|s| !s.is_empty()),
            "host" => c.host = lines.first().map(|l| l.trim().to_string()).filter(|s| !s.is_empty()),
            "ps" => c.procs = parse_ps(&lines),
            _ => {}
        }
    }
    if c.cpu.is_none() || c.mem_total == 0 {
        return Err("读不到 /proc/stat 或 /proc/meminfo".into());
    }
    Ok(c)
}

/// 本次计数 + 上一次（与间隔秒数）→ 一次采样。没有上一次时 CPU 与网速为 None
pub fn snapshot(cur: &Counters, prev: Option<(&Counters, f64)>) -> Snapshot {
    let cpu = prev.and_then(|(p, _)| {
        let ((t1, i1), (t0, i0)) = (cur.cpu?, p.cpu?);
        let dt = t1.checked_sub(t0)?;
        // 计数器回绕 / 主机重启过（总量变小）就算不出来，这一次留空
        (dt > 0).then(|| (100.0 * (1.0 - i1.saturating_sub(i0) as f64 / dt as f64)).clamp(0.0, 100.0))
    });
    let net = prev.and_then(|(p, secs)| {
        let ((r1, t1), (r0, t0)) = (cur.net?, p.net?);
        (secs > 0.0 && r1 >= r0 && t1 >= t0).then(|| ((r1 - r0) as f64 / secs, (t1 - t0) as f64 / secs))
    });
    Snapshot {
        host: cur.host.clone(),
        os: cur.os.clone(),
        uptime: cur.uptime,
        cores: cur.cores,
        cpu,
        load: cur.load,
        mem_total: cur.mem_total,
        mem_used: cur.mem_total.saturating_sub(cur.mem_available),
        swap_total: cur.swap_total,
        swap_used: cur.swap_total.saturating_sub(cur.swap_free),
        net,
        disks: cur.disks.clone(),
        procs: top_procs(cur.procs.clone(), ProcSort::Memory),
        procs_by: ProcSort::Memory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ubuntu 22.04（procps 的 ps）上抓的一次，删了些行
    const UBUNTU: &str = "@@stat
cpu  10132153 290696 3084719 46828483 16683 0 25195 0 0 0
8
@@meminfo
MemTotal:       16314584 kB
MemFree:         1186720 kB
MemAvailable:    9372300 kB
Buffers:          462044 kB
Cached:          7536628 kB
SwapTotal:       2097148 kB
SwapFree:        1048574 kB
@@loadavg
0.52 0.58 0.59 2/1123 4166232
@@uptime
3519262.85 27382711.34
@@net
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 2776770   11307    0    0    0     0          0         0  2776770   11307    0    0    0     0       0          0
  eth0: 1215645  139921    0    0    0     0          0         0   977010   11008    0    0    0     0       0          0
docker0:  100000     100    0    0    0     0          0         0   200000     100    0    0    0     0       0          0
@@df
Filesystem     1024-blocks      Used Available Capacity Mounted on
tmpfs              1631460      2156   1629304       1% /run
/dev/sda2        102626232  61205288  36161680      63% /
/dev/sdb1        961302496 512000000 400000000      57% /data/My Disk
/dev/loop0           64768     64768         0     100% /snap/core20/1405
@@os
\"Ubuntu 22.04.3 LTS\"
@@host
devbox
@@ps
 1310720 node
   10240 sshd
       0 kworker/0:1
  204800 postgres
";

    /// alpine 容器（busybox）：ps 走了第二种写法，带表头、带单位后缀
    const ALPINE: &str = "@@stat
cpu  4705 0 3311 1128830 210 0 82 0 0 0
4
@@meminfo
MemTotal:        8130496 kB
MemFree:         6500000 kB
MemAvailable:    7200000 kB
SwapTotal:             0 kB
SwapFree:              0 kB
@@loadavg
0.00 0.01 0.05 1/180 120
@@uptime
11853.31 46600.78
@@net
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
  eth0:   57630     411    0    0    0     0          0         0    40164     296    0    0    0     0       0          0
@@df
Filesystem           1024-blocks    Used Available Capacity Mounted on
overlay               61202244  21000000  37069000  36% /
tmpfs                    65536         0     65536   0% /dev
@@os
\"Alpine Linux v3.20\"
@@host
3f2a9c1
@@ps
RSS  COMMAND
1.2m sshd
 932 sh
  12m python3
";

    #[test]
    fn ubuntu_sample() {
        let c = parse_probe(UBUNTU).unwrap();
        assert_eq!(c.cpu, Some((10132153 + 290696 + 3084719 + 46828483 + 16683 + 25195, 46828483 + 16683)));
        assert_eq!(c.cores, 8);
        assert_eq!(c.mem_total, 16314584 * 1024);
        assert_eq!(c.mem_available, 9372300 * 1024);
        assert_eq!(c.load, Some([0.52, 0.58, 0.59]));
        assert_eq!(c.uptime, Some(3519262));
        assert_eq!(c.net, Some((1215645, 977010)), "回环与 docker0 不计");
        assert_eq!(c.disks.iter().map(|d| d.mount.as_str()).collect::<Vec<_>>(), vec!["/", "/data/My Disk"]);
        assert_eq!(c.disks[0].total, 102626232 * 1024);
        assert_eq!(c.os.as_deref(), Some("Ubuntu 22.04.3 LTS"));
        assert_eq!(c.host.as_deref(), Some("devbox"));
        assert_eq!(c.procs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["node", "sshd", "postgres"]);
    }

    #[test]
    fn alpine_busybox_sample() {
        let c = parse_probe(ALPINE).unwrap();
        assert_eq!(c.cores, 4);
        assert_eq!(c.swap_total, 0);
        assert_eq!(c.disks.len(), 1);
        assert_eq!(c.disks[0].mount, "/");
        let names: Vec<_> = c.procs.iter().map(|p| (p.name.as_str(), p.mem)).collect();
        assert_eq!(names, vec![("sshd", (1.2 * 1024.0 * 1024.0) as u64), ("sh", 932 * 1024), ("python3", 12 * 1024 * 1024)]);
        let s = snapshot(&c, None);
        assert_eq!(s.procs[0].name, "python3", "远端按内存排");
        assert_eq!(s.procs_by, ProcSort::Memory);
    }

    #[test]
    fn deltas_between_two_probes() {
        let mut a = parse_probe(UBUNTU).unwrap();
        a.cpu = Some((1000, 800));
        a.net = Some((10_000, 2_000));
        let mut b = a.clone();
        b.cpu = Some((1200, 850));
        b.net = Some((30_000, 4_000));
        assert_eq!(snapshot(&b, None).cpu, None);
        let s = snapshot(&b, Some((&a, 2.0)));
        assert_eq!(s.cpu, Some(75.0));
        assert_eq!(s.net, Some((10_000.0, 1_000.0)));
        assert_eq!(s.mem_used, (16314584 - 9372300) * 1024);
        assert_eq!(s.swap_used, (2097148 - 1048574) * 1024);
        // 主机重启过：计数变小，这一次算不出来
        let s = snapshot(&a, Some((&b, 2.0)));
        assert_eq!((s.cpu, s.net), (None, None));
    }

    #[test]
    fn old_kernel_without_mem_available() {
        let c = parse_probe("@@stat\ncpu 1 2 3 4\n1\n@@meminfo\nMemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 250 kB\n").unwrap();
        assert_eq!(c.mem_available, 400 * 1024);
    }

    #[test]
    fn not_linux() {
        assert!(parse_probe("@@stat\n@@meminfo\n").is_err());
        assert!(parse_probe("").is_err());
    }
}
