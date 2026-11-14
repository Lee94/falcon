//! `falcon-app` 二进制入口（ADR 0028）。

fn main() {
    if let Err(err) = falcon_app::run_cli() {
        eprintln!("falcon-app error: {err:#}");
        std::process::exit(1);
    }
}
