//! 手动验证真库：`cargo run -p falcon-tailnet --example smoke [-- <状态目录>]`
//!
//! 不带参数只加载库、打印状态（不联网）。带状态目录就起一个节点，每秒打印一次状态，直到
//! running 或 Ctrl-C；第一次会打出登录 URL。用完 `-- <目录> --logout` 退出登录并清掉目录。

use std::time::Duration;

use falcon_tailnet::{StartConfig, State, Tailnet};

fn main() {
    let tn = match Tailnet::global() {
        Ok(tn) => tn,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("已加载 {}", tn.path().display());
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first() else {
        println!("{:?}", tn.status());
        return;
    };
    if args.iter().any(|a| a == "--logout") {
        tn.start(&config(dir)).expect("起节点");
        println!("logout：{:?}", tn.logout(dir.as_ref()));
        return;
    }
    tn.start(&config(dir)).expect("起节点");
    let mut last = None;
    loop {
        let st = tn.status().expect("状态");
        if last.as_ref() != Some(&st) {
            println!("{}", serde_json::to_string_pretty(&st).unwrap());
            last = Some(st.clone());
        }
        if st.state == State::Running {
            println!("代理：{:?}", tn.proxy());
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    tn.stop();
}

fn config(dir: &str) -> StartConfig {
    StartConfig { state_dir: dir.into(), hostname: "falcon-smoke".into(), control_url: None, auth_key: None }
}
