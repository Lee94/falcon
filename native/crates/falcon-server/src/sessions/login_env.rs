//! 本地 PTY 的基底环境：login shell 环境解析 + locale 兜底。移植自
//! `packages/server/src/sessions/loginEnv.ts` 的纯函数部分（命令构造、`env -0` 输出解析、
//! 合并与补 LANG、locale 挑选）。
//!
//! 留到 S3 的（执行 / 进程 / 读本机环境）：`runLoginShell`（spawn 探测 shell、超时与输出上限）、
//! `resolveLocalBaseEnv` / `resetLocalBaseEnv` / `doResolve`（进程级缓存与并行探测）、
//! `loginProbeShell` 里读 passwd 条目与 `process.env.SHELL` 的那一步（挑选规则见
//! [`pick_login_probe_shell`]）。
//!
//! 背景：PTY 环境此前直接摊开后端进程的 process.env，而 process.env 取决于
//! 后端被谁启动——launchd / IDE / 精简 shell 启动时 PATH 往往缺
//! /opt/homebrew/bin 等，且完全没有 LANG（GUI / 服务进程的常态）。pane 里的
//! shell 又是非 login 方式起的（Zellij 的 --default-shell 是 PathBuf，带不了
//! -l；非持久会话也是 `pty.spawn(shell, [])`），不会执行 ~/.zprofile 与
//! /etc/zprofile（path_helper），PATH 无从自我修复；没有 UTF-8 locale 时
//! zsh 的行编辑把中文按单字节回显，输入中文即乱码。
//!
//! 做法学 VS Code 的 shell environment resolution：起一个 login + interactive
//! 的 shell 跑 env，拿它的环境覆盖到 process.env 上作为本地 PTY 的基底；
//! 合并后仍无 locale 时按平台探测出一个 UTF-8 locale 注入 LANG。
//! 任一环节失败都静默退回 process.env——这套是增强，不是门禁。
//!
//! 每个后端进程只解析一次，结果缓存（与 prepareLocalZellij 同节奏）。代价：
//! 用户改了 rc 文件要重启后端才生效——和改了 rc 要重开终端窗口是同一个心智。
//! 注意持久会话的 env 在 Zellij server 首次 attach 时就冻住了，本模块的修正
//! 只影响之后新建的会话，已存在的持久会话要删掉重建。
//!
//! 环境变量是**有序**的 `(名, 值)` 列表（TS 对象的插入顺序；同名键就地改值）。

use std::collections::HashMap;
use std::future::Future;
use std::sync::LazyLock;

use regex::Regex;

use crate::exec::ExecResult;
use crate::term_env::js;

pub const LOGIN_ENV_BEGIN: &str = "__FALCON_LOGIN_ENV_BEGIN__";
pub const LOGIN_ENV_END: &str = "__FALCON_LOGIN_ENV_END__";

/// rc 疯狂输出（死循环、二进制喷屏）时的止损上限（runLoginShell 用，S3）
pub const PROBE_OUTPUT_CAP: usize = 8 * 1024 * 1024;

/// rc 慢的大头是 nvm / conda 一类初始化，实测多在 1s 内；10s 已是病态（runLoginShell 用，S3）
pub const PROBE_TIMEOUT_MS: u64 = 10_000;

/// FALCON_RESOLVING_ENV=1 打给用户的 rc：想跳过重活（nvm、耗时的补全初始化）
/// 的可以用它判断，等价于 VS Code 的 VSCODE_RESOLVING_ENVIRONMENT（runLoginShell 用，S3）
pub const RESOLVING_ENV_VAR: &str = "FALCON_RESOLVING_ENV";

/// 探测命令的 argv（跟在 shell 路径后面）。
///
/// - `-i` 有意为之：不少人把 PATH / nvm 写在 .zshrc 且用 `[[ $- == *i* ]]`
///   守卫，纯 login 非交互会漏掉它们。
/// - 分开传 `-i -l -c` 而不是合并 `-ilc`：短参合并的支持各 shell 不一。
/// - `env -0` 用 NUL 分隔，值里的换行不会破坏解析；`||` 兜住不认 -0 的老
///   平台退回按行输出。stderr 不参与解析所以**不加** 2>/dev/null——那是
///   POSIX 语法，会把 csh 系的整条命令炸掉。
/// - 前后 echo 标记：交互 rc（greeting、nvm 提示）会污染 stdout，zlogout
///   还可能在命令之后再打一段，只取两个标记之间的部分。
/// - tcsh（-l 必须单独出现）、老 nushell（不认 ||）等会整条失败——探测失败
///   只是退回 process.env，不用为它们做特化。
pub fn login_env_probe_args() -> Vec<String> {
    vec![
        "-i".into(),
        "-l".into(),
        "-c".into(),
        format!("echo {LOGIN_ENV_BEGIN}; /usr/bin/env -0 || /usr/bin/env; echo {LOGIN_ENV_END}"),
    ]
}

/// 探测用的登录 shell。优先 passwd 条目而不是 $SHELL：launchd / 服务环境里
/// SHELL 往往缺失，process.env.SHELL 缺时 defaultLocalShell 会退到 /bin/bash，
/// 而 zsh 用户的 PATH 都写在 zsh 的 rc 里——用错 shell 探出来的环境是空的。
/// userInfo() 在极端环境（容器里无 passwd 条目）会抛，兜住。
///
/// 这里只留挑选规则（TS 的 `||`：空串与缺失一样顺延下一级）；读 passwd 与环境变量在 S3。
pub fn pick_login_probe_shell(passwd_shell: Option<&str>, env_shell: Option<&str>) -> String {
    passwd_shell.filter(|s| !s.is_empty()).or(env_shell.filter(|s| !s.is_empty())).unwrap_or("/bin/bash").to_string()
}

/// 按行回退时认 `KEY=` 开头的行。JS 的 `.` 不匹配 \r、U+2028、U+2029（Rust 的 `.` 会），
/// 这里显式排除，带 \r 的行与 TS 一样当续行。
static ENV_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("^([A-Za-z_][A-Za-z0-9_]*)=([^\r\n\u{2028}\u{2029}]*)$").expect("ENV_LINE"));

/// 有序 env 的赋值：已有同名键就地改值（保持原位置，与 JS 对象一致），没有就追加
fn env_set(env: &mut Vec<(String, String)>, key: &str, value: String) {
    match env.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => env.push((key.to_string(), value)),
    }
}

/// 有序 env 里取一个键
pub fn env_get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// 从探测 stdout 里解析 env。标记缺失或没解出任何变量时返回 None。
/// begin 取第一次出现、end 取最后一次：rc 输出在前、zlogout 输出在后。
pub fn parse_login_env(stdout: &str) -> Option<Vec<(String, String)>> {
    let begin = stdout.find(LOGIN_ENV_BEGIN)?;
    let start = begin + LOGIN_ENV_BEGIN.len();
    let end = stdout.rfind(LOGIN_ENV_END)?;
    if end <= start {
        return None;
    }

    let mut body = &stdout[start..end];
    // 去掉 begin 标记 echo 补的换行；env 输出自身以分隔符（\0 或 \n）结尾
    if let Some(rest) = body.strip_prefix('\n') {
        body = rest;
    }

    let mut env: Vec<(String, String)> = Vec::new();
    if body.contains('\0') {
        // env -0：NUL 分隔，值可以含换行
        for entry in body.split('\0') {
            if entry.is_empty() {
                continue;
            }
            // TS 的 `eq <= 0`：没有等号或等号打头都跳过
            match entry.find('=') {
                Some(eq) if eq > 0 => env_set(&mut env, &entry[..eq], entry[eq + 1..].to_string()),
                _ => continue,
            }
        }
    } else {
        // 退化的按行输出：不像 `KEY=` 开头的行当作上一个值的续行
        let mut lines: Vec<&str> = body.split('\n').collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        let mut key: Option<String> = None;
        for line in lines {
            if let Some(m) = ENV_LINE.captures(line) {
                let k = m[1].to_string();
                env_set(&mut env, &k, m[2].to_string());
                key = Some(k);
            } else if let Some(k) = &key
                && let Some(slot) = env.iter_mut().find(|(ek, _)| ek == k)
            {
                slot.1.push('\n');
                slot.1.push_str(line);
            }
        }
    }
    if env.is_empty() { None } else { Some(env) }
}

/// login env 里对新 PTY 无意义或有害的键，合并时丢弃：
/// SHLVL/PWD/OLDPWD/_ 是探测 shell 自己的运行痕迹，
/// FALCON_RESOLVING_ENV 是我们打给 rc 的标记（见 runLoginShell）。
const PROBE_NOISE: [&str; 5] = ["SHLVL", "PWD", "OLDPWD", "_", RESOLVING_ENV_VAR];

/// LANG / LC_ALL / LC_CTYPE 任一非空（JS 真值：空串不算）
pub fn has_locale(env: &[(String, String)]) -> bool {
    ["LANG", "LC_ALL", "LC_CTYPE"].iter().any(|k| env_get(env, k).is_some_and(|v| !v.is_empty()))
}

/// 组合基底环境：process.env 打底，login env 覆盖（PATH / LANG 以登录环境
/// 为准，这正是解析的目的；只存在于 process.env 的键——比如用户
/// `FOO=bar falcon` 启动时注入的——原样保留）。合并后仍无任何 locale 线索
/// 才注入 fallbackLang；LANG=C 这类显式设置视为用户的选择，不动。
pub fn merge_base_env(
    process_env: &[(String, String)],
    login_env: Option<&[(String, String)]>,
    fallback_lang: Option<&str>,
) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = Vec::with_capacity(process_env.len());
    for (k, v) in process_env {
        env_set(&mut merged, k, v.clone());
    }
    if let Some(login_env) = login_env {
        for (k, v) in login_env {
            if PROBE_NOISE.contains(&k.as_str()) {
                continue;
            }
            env_set(&mut merged, k, v.clone());
        }
    }
    if let Some(lang) = fallback_lang.filter(|l| !l.is_empty())
        && !has_locale(&merged)
    {
        env_set(&mut merged, "LANG", lang.to_string());
    }
    merged
}

static APPLE_LOCALE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("^([a-z]{2,3})(?:-[A-Za-z]+)?_([A-Z]{2})").expect("APPLE_LOCALE"));

/// AppleLocale → POSIX locale 名。形如 `zh_CN`、`en_US`，还可能带脚本段
/// （`zh-Hans_CN`）或区域修饰（`zh_CN@rg=uszzzz`），只取 语言_地区。
pub fn apple_locale_to_posix(raw: &str) -> Option<String> {
    let m = APPLE_LOCALE.captures(js::trim(raw))?;
    Some(format!("{}_{}", &m[1], &m[2]))
}

/// 在 `locale -a` 的输出里找第一个可用候选。返回列表里的原始拼写：
/// macOS 列出的是 `zh_CN.UTF-8`，glibc 列出的是 `zh_CN.utf8`，
/// 两种拼法系统各自都认，但用列表原文最稳。
pub fn pick_utf8_locale(candidates: &[Option<&str>], locale_listing: &str) -> Option<String> {
    // TS 的 String.replace(字符串, …) 只换第一处，这里同样 replacen(…, 1)
    let norm = |s: &str| js::trim(s).to_lowercase().replacen("utf-8", "utf8", 1);
    let mut available: HashMap<String, &str> = HashMap::new();
    for line in locale_listing.split('\n') {
        let t = js::trim(line);
        if !t.is_empty() {
            available.insert(norm(t), t);
        }
    }
    candidates.iter().flatten().find_map(|c| available.get(&norm(c)).map(|hit| hit.to_string()))
}

/// 本机平台的 Node 叫法（`process.platform`）：macOS 是 `darwin`、Windows 是 `win32`，
/// 其余与 Rust 的 `std::env::consts::OS` 同名
pub fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// 挑一个用于 LANG 兜底的 UTF-8 locale。
///
/// macOS 学 Terminal.app：从系统区域设置（AppleLocale）推导——Terminal 里
/// 用户看到的 LANG 本来就是它注入的，login shell 解析拿不到（这正是 locale
/// 兜底存在的原因）；推不出或不可用时退 en_US.UTF-8（macOS 恒有）。
/// Linux 首选 C.UTF-8（glibc ≥ 2.35 恒有，更老的发行版也普遍预置），
/// 语言中性、不赌 locale-gen 生成过什么。`locale -a` 本身失败时按平台
/// 直接给硬值——错杀（系统真没有该 locale）的后果只是 setlocale 失败退回
/// C，不会比不注入更糟。
///
/// `exec` 是 ExecFn（非零退出码是正常返回值），`platform` 用 Node 的叫法（见 [`node_platform`]）。
/// TS 的返回类型写着 `string | null`，但每条路径都有值，这里直接返回 `String`。
pub async fn detect_fallback_lang<F, Fut>(mut exec: F, platform: &str) -> String
where
    F: FnMut(&'static str) -> Fut,
    Fut: Future<Output = ExecResult>,
{
    let mut candidates: Vec<Option<String>> = Vec::new();
    if platform == "darwin" {
        let res = exec("defaults read -g AppleLocale").await;
        let posix = if res.code == Some(0) { apple_locale_to_posix(&res.stdout) } else { None };
        candidates.push(posix.map(|p| format!("{p}.UTF-8")));
        candidates.push(Some("en_US.UTF-8".into()));
    } else {
        candidates.push(Some("C.UTF-8".into()));
        candidates.push(Some("en_US.UTF-8".into()));
    }

    let listing = exec("locale -a").await;
    let refs: Vec<Option<&str>> = candidates.iter().map(Option::as_deref).collect();
    let picked = pick_utf8_locale(&refs, if listing.code == Some(0) { &listing.stdout } else { "" });
    if let Some(p) = picked {
        return p;
    }
    if platform == "darwin" { "en_US.UTF-8".into() } else { "C.UTF-8".into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{Ready, ready};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // describe("loginEnvProbeArgs")

    #[test]
    fn login_env_probe_args_passes_i_l_c_separately() {
        // 分开传 -i -l -c，不合并短参（fish 等的支持不一）
        let args = login_env_probe_args();
        assert_eq!(&args[..3], ["-i", "-l", "-c"]);
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn login_env_probe_args_script_has_markers_and_env0_fallback_without_csh_breaking_redirect() {
        // 脚本带前后标记与 env -0 回退，且没有 csh 会炸的 2> 重定向
        let script = &login_env_probe_args()[3];
        assert!(script.contains(LOGIN_ENV_BEGIN));
        assert!(script.contains(LOGIN_ENV_END));
        assert!(script.contains("/usr/bin/env -0 || /usr/bin/env"));
        assert!(!script.contains("2>"));
    }

    // describe("parseLoginEnv")

    #[test]
    fn parse_login_env_nul_mode_takes_only_the_part_between_markers() {
        // NUL 模式：取标记之间的部分，rc 与 zlogout 的污染都被丢掉
        let stdout = format!(
            "Last login: today\nnvm is slow...\n{LOGIN_ENV_BEGIN}\nPATH=/opt/homebrew/bin:/usr/bin\0HOME=/Users/fay\0{LOGIN_ENV_END}\ngoodbye from zlogout\n"
        );
        assert_eq!(
            parse_login_env(&stdout),
            Some(env(&[("PATH", "/opt/homebrew/bin:/usr/bin"), ("HOME", "/Users/fay")]))
        );
    }

    #[test]
    fn parse_login_env_nul_mode_keeps_newlines_and_equals_in_values() {
        // NUL 模式：值里的换行与等号原样保留
        let stdout = format!("{LOGIN_ENV_BEGIN}\nMULTI=line1\nline2\0EQ=a=b=c\0{LOGIN_ENV_END}\n");
        assert_eq!(parse_login_env(&stdout), Some(env(&[("MULTI", "line1\nline2"), ("EQ", "a=b=c")])));
    }

    #[test]
    fn parse_login_env_line_fallback_joins_continuation_lines() {
        // 按行回退（env 不认 -0）：非 KEY= 开头的行并入上一个值
        let stdout =
            format!("{LOGIN_ENV_BEGIN}\nPATH=/usr/bin\nMULTI=line1\nline2\nLANG=en_US.UTF-8\n{LOGIN_ENV_END}\n");
        assert_eq!(
            parse_login_env(&stdout),
            Some(env(&[("PATH", "/usr/bin"), ("MULTI", "line1\nline2"), ("LANG", "en_US.UTF-8")]))
        );
    }

    #[test]
    fn parse_login_env_returns_none_without_markers_or_variables() {
        // 标记缺失或解不出变量时返回 null，让调用方退回 process.env
        assert_eq!(parse_login_env("no markers at all"), None);
        assert_eq!(parse_login_env(&format!("{LOGIN_ENV_BEGIN}\n")), None);
        assert_eq!(parse_login_env(&format!("{LOGIN_ENV_BEGIN}\n\n{LOGIN_ENV_END}\n")), None);
    }

    // describe("mergeBaseEnv")

    #[test]
    fn merge_base_env_login_env_overrides_process_env_and_keeps_process_only_keys() {
        // login env 覆盖 process.env（PATH 以登录环境为准），process.env 独有的键保留
        let merged = merge_base_env(
            &env(&[("PATH", "/usr/bin"), ("FOO", "bar")]),
            Some(&env(&[("PATH", "/opt/homebrew/bin:/usr/bin"), ("EDITOR", "vim")])),
            None,
        );
        assert_eq!(env_get(&merged, "PATH"), Some("/opt/homebrew/bin:/usr/bin"));
        assert_eq!(env_get(&merged, "FOO"), Some("bar"));
        assert_eq!(env_get(&merged, "EDITOR"), Some("vim"));
    }

    #[test]
    fn merge_base_env_drops_probe_shell_traces_and_the_resolving_marker() {
        // 探测 shell 的运行痕迹（SHLVL/PWD/…）与解析标记不进基底
        let merged = merge_base_env(
            &env(&[("PWD", "/srv/falcon")]),
            Some(&env(&[
                ("SHLVL", "2"),
                ("PWD", "/Users/fay"),
                ("OLDPWD", "/"),
                ("_", "/usr/bin/env"),
                ("FALCON_RESOLVING_ENV", "1"),
                ("PATH", "/usr/bin"),
            ])),
            None,
        );
        assert_eq!(env_get(&merged, "SHLVL"), None);
        assert_eq!(env_get(&merged, "PWD"), Some("/srv/falcon"));
        assert_eq!(env_get(&merged, "OLDPWD"), None);
        assert_eq!(env_get(&merged, "_"), None);
        assert_eq!(env_get(&merged, "FALCON_RESOLVING_ENV"), None);
    }

    #[test]
    fn merge_base_env_injects_fallback_only_without_lang_lc_all_lc_ctype() {
        // locale 兜底只在 LANG/LC_ALL/LC_CTYPE 全缺时注入
        let lang = |pe: &[(&str, &str)], le: Option<&[(&str, &str)]>, fb: Option<&str>| {
            let le = le.map(env);
            env_get(&merge_base_env(&env(pe), le.as_deref(), fb), "LANG").map(str::to_string)
        };
        assert_eq!(lang(&[], None, Some("zh_CN.UTF-8")).as_deref(), Some("zh_CN.UTF-8"));
        // 显式 LANG=C 是用户的选择，不覆盖
        assert_eq!(lang(&[("LANG", "C")], None, Some("zh_CN.UTF-8")).as_deref(), Some("C"));
        assert_eq!(lang(&[("LC_CTYPE", "UTF-8")], None, Some("zh_CN.UTF-8")), None);
        // login env 带来的 locale 同样挡住兜底
        assert_eq!(lang(&[], Some(&[("LANG", "ja_JP.UTF-8")]), Some("zh_CN.UTF-8")).as_deref(), Some("ja_JP.UTF-8"));
        assert_eq!(lang(&[], None, None), None);
    }

    // describe("appleLocaleToPosix")

    #[test]
    fn apple_locale_to_posix_normalizes_to_language_region() {
        // 裸 语言_地区 与带脚本段 / 区域修饰的形态都归一到 语言_地区
        assert_eq!(apple_locale_to_posix("zh_CN").as_deref(), Some("zh_CN"));
        assert_eq!(apple_locale_to_posix("en_US").as_deref(), Some("en_US"));
        assert_eq!(apple_locale_to_posix("zh-Hans_CN").as_deref(), Some("zh_CN"));
        assert_eq!(apple_locale_to_posix("zh_CN@rg=uszzzz").as_deref(), Some("zh_CN"));
        assert_eq!(apple_locale_to_posix("yue-Hant_HK").as_deref(), Some("yue_HK"));
    }

    #[test]
    fn apple_locale_to_posix_returns_none_when_unparseable() {
        // 解析不出就 null（AppleLocale 也可能只有语言段）
        assert_eq!(apple_locale_to_posix("zh"), None);
        assert_eq!(apple_locale_to_posix(""), None);
        assert_eq!(apple_locale_to_posix("garbage"), None);
    }

    // describe("pickUtf8Locale")

    #[test]
    fn pick_utf8_locale_matches_both_spellings_and_returns_the_listing_original() {
        // UTF-8 与 utf8 两种拼写互认，返回 locale -a 的原始拼写
        // macOS 的列表拼写
        assert_eq!(
            pick_utf8_locale(&[Some("zh_CN.UTF-8")], "en_US.UTF-8\nzh_CN.UTF-8\nzh_CN.GB18030\n").as_deref(),
            Some("zh_CN.UTF-8")
        );
        // glibc 的列表拼写：候选写 UTF-8 也要命中，且返回列表原文
        assert_eq!(pick_utf8_locale(&[Some("en_US.UTF-8")], "C\nC.utf8\nen_US.utf8\n").as_deref(), Some("en_US.utf8"));
    }

    #[test]
    fn pick_utf8_locale_takes_the_first_available_candidate_in_order() {
        // 按候选顺序取第一个可用项，null 候选跳过，全不可用返回 null
        assert_eq!(
            pick_utf8_locale(&[None, Some("xx_XX.UTF-8"), Some("en_US.UTF-8")], "en_US.UTF-8\n").as_deref(),
            Some("en_US.UTF-8")
        );
        assert_eq!(pick_utf8_locale(&[Some("zh_CN.UTF-8")], ""), None);
    }

    // describe("detectFallbackLang")

    /// 假的 ExecFn：表里有的命令回表里的结果，其余回退出码 1
    fn fake(table: &[(&str, i32, &str)]) -> impl FnMut(&'static str) -> Ready<ExecResult> {
        let table: Vec<(String, ExecResult)> = table
            .iter()
            .map(|(cmd, code, stdout)| {
                (cmd.to_string(), ExecResult { code: Some(*code), stdout: stdout.to_string(), stderr: String::new() })
            })
            .collect();
        move |cmd| {
            ready(table.iter().find(|(c, _)| c == cmd).map(|(_, r)| r.clone()).unwrap_or(ExecResult {
                code: Some(1),
                stdout: String::new(),
                stderr: String::new(),
            }))
        }
    }

    /// 假 exec 回的都是现成的 future，轮询一次就该完成
    fn block_on<T>(fut: impl Future<Output = T>) -> T {
        let mut fut = pin!(fut);
        match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("fake exec should resolve immediately"),
        }
    }

    #[test]
    fn detect_fallback_lang_darwin_prefers_apple_locale_verified_by_locale_a() {
        // darwin：AppleLocale 推导优先，locale -a 验证通过才用
        let exec =
            fake(&[("defaults read -g AppleLocale", 0, "zh_CN\n"), ("locale -a", 0, "en_US.UTF-8\nzh_CN.UTF-8\n")]);
        assert_eq!(block_on(detect_fallback_lang(exec, "darwin")), "zh_CN.UTF-8");
    }

    #[test]
    fn detect_fallback_lang_darwin_falls_back_to_en_us_when_region_locale_is_missing() {
        // darwin：区域 locale 不在列表里时退 en_US.UTF-8
        let exec = fake(&[("defaults read -g AppleLocale", 0, "xx_XX\n"), ("locale -a", 0, "en_US.UTF-8\n")]);
        assert_eq!(block_on(detect_fallback_lang(exec, "darwin")), "en_US.UTF-8");
    }

    #[test]
    fn detect_fallback_lang_linux_prefers_language_neutral_c_utf8() {
        // linux：首选语言中性的 C.UTF-8
        let exec = fake(&[("locale -a", 0, "C\nC.utf8\nPOSIX\nen_US.utf8\n")]);
        assert_eq!(block_on(detect_fallback_lang(exec, "linux")), "C.utf8");
    }

    #[test]
    fn detect_fallback_lang_gives_hard_values_when_locale_a_fails() {
        // locale -a 失败时按平台给硬值，绝不返回 null
        assert_eq!(block_on(detect_fallback_lang(fake(&[]), "darwin")), "en_US.UTF-8");
        assert_eq!(block_on(detect_fallback_lang(fake(&[]), "linux")), "C.UTF-8");
    }

    // 以下不在 TS 测试里：与 TS 实跑（tsx）对出来的边角

    #[test]
    fn edge_cases_match_ts() {
        // CRLF 的行：JS 的 `.` 不吃 \r，整行当续行
        let stdout = format!("{LOGIN_ENV_BEGIN}\nA=1\r\nB=2\n{LOGIN_ENV_END}");
        assert_eq!(parse_login_env(&stdout), Some(env(&[("B", "2")])));
        // 重复的键：就地改值、位置不变
        let stdout = format!("{LOGIN_ENV_BEGIN}\nA=1\0B=2\0A=3\0=x\0noeq\0{LOGIN_ENV_END}");
        assert_eq!(parse_login_env(&stdout), Some(env(&[("A", "3"), ("B", "2")])));
        // end 在 begin 之前
        assert_eq!(parse_login_env(&format!("{LOGIN_ENV_END}{LOGIN_ENV_BEGIN}\nA=1\n")), None);
        // LANG 存在但为空：算没有 locale，兜底就地写进去
        let merged = merge_base_env(&env(&[("LANG", ""), ("X", "1")]), None, Some("C.UTF-8"));
        assert_eq!(merged, env(&[("LANG", "C.UTF-8"), ("X", "1")]));
        // 探测 shell：空串与缺失一样顺延
        assert_eq!(pick_login_probe_shell(Some(""), Some("/bin/zsh")), "/bin/zsh");
        assert_eq!(pick_login_probe_shell(Some("/bin/fish"), Some("/bin/zsh")), "/bin/fish");
        assert_eq!(pick_login_probe_shell(None, Some("")), "/bin/bash");
    }
}
