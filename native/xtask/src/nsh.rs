//! NSH 六端统一客户端开发、检查与基准任务（ADR 0028）。

use anyhow::Result;

use crate::util;

/// `cargo xtask tui [参数...]`：在当前终端运行 `falcon-app --tui`。
pub fn tui(args: &[String]) -> Result<()> {
    let native = util::native();
    let mut cargo_args: Vec<String> = vec![
        "run".into(),
        "-p".into(),
        "falcon-app".into(),
        "--".into(),
        "--tui".into(),
    ];
    cargo_args.extend(args.iter().cloned());
    util::run("cargo", &cargo_args, Some(&native))
}

/// `cargo xtask nsh-check`：局部编译检查 `falcon-nsh` 与 `falcon-app`（含测试目标）。
pub fn check(args: &[String]) -> Result<()> {
    let native = util::native();
    let mut check_nsh = vec![
        "check".to_string(),
        "-p".to_string(),
        "falcon-nsh".to_string(),
        "--tests".to_string(),
    ];
    check_nsh.extend(args.iter().cloned());
    util::run("cargo", &check_nsh, Some(&native))?;

    let mut check_app = vec![
        "check".to_string(),
        "-p".to_string(),
        "falcon-app".to_string(),
        "--tests".to_string(),
    ];
    check_app.extend(args.iter().cloned());
    util::run("cargo", &check_app, Some(&native))
}

/// `cargo xtask nsh-bench`：在 `--release` 下运行 `FalconTerminal` 30 万行高吞吐帧时延门禁基准。
pub fn bench(args: &[String]) -> Result<()> {
    let native = util::native();
    let mut bench_args = vec![
        "test".to_string(),
        "--release".to_string(),
        "-p".to_string(),
        "falcon-nsh".to_string(),
        "phase0_gate_benchmark_300k_mixed_cjk_box_ansi_under_4ms_p95".to_string(),
        "--".to_string(),
        "--nocapture".to_string(),
    ];
    bench_args.extend(args.iter().cloned());
    util::run("cargo", &bench_args, Some(&native))
}
