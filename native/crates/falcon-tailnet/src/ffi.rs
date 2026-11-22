//! dlopen 与 C ABI 那一层。导出符号见 `native/tailnet/main.go`。

use std::ffi::{CStr, CString, OsString, c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::{ABI_VERSION, Error, InterfaceList, LIB_STEM, LoadError, ProxyInfo, StartConfig, Status};

type AbiVersionFn = unsafe extern "C" fn() -> c_int;
type StrInFn = unsafe extern "C" fn(*const c_char) -> *mut c_char;
type StrOutFn = unsafe extern "C" fn() -> *mut c_char;
type VoidFn = unsafe extern "C" fn();
type FreeFn = unsafe extern "C" fn(*mut c_char);

/// 加载好的库。只能经 [`Tailnet::global`] / [`Tailnet::open`] 拿到 `&'static` 引用：库永不卸载
pub struct Tailnet {
    path: PathBuf,
    start: StrInFn,
    status: StrOutFn,
    proxy: StrOutFn,
    stop: VoidFn,
    logout: StrOutFn,
    set_interfaces: StrInFn,
    free: FreeFn,
}

#[derive(Deserialize)]
struct GoError {
    error: String,
}

/// 开发构建时 `cargo xtask tailnet` 产物所在的三元组目录
const DEV_TRIPLE: Option<&str> = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("aarch64-apple-darwin")
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    Some("x86_64-apple-darwin")
} else if cfg!(all(windows, target_arch = "x86_64")) {
    Some("x86_64-pc-windows-msvc")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("x86_64-unknown-linux-gnu")
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    Some("aarch64-unknown-linux-gnu")
} else {
    None
};

/// 按顺序找库的位置。设了 `FALCON_TAILNET_LIB` 就只认它；否则依次是：
///
/// - 安卓：裸文件名，交给系统链接器在 App 的 native 库目录里找（APK 的 `jniLibs/<abi>/`）；
/// - 可执行文件同目录（Windows 安装包、Linux 开发）；
/// - macOS：`Falcon.app/Contents/Frameworks/`（可执行文件在 `Contents/MacOS/`）；
/// - 开发构建（debug）：`native/target-tailnet/<三元组>/`。
pub fn candidates() -> Vec<PathBuf> {
    candidates_from(std::env::var_os("FALCON_TAILNET_LIB"), std::env::current_exe().ok())
}

fn candidates_from(env: Option<OsString>, exe: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(p) = env.filter(|p| !p.is_empty()) {
        return vec![PathBuf::from(p)];
    }
    let name = PathBuf::from(libloading::library_filename(LIB_STEM));
    let mut out = Vec::new();
    if cfg!(target_os = "android") {
        out.push(name.clone());
    }
    if let Some(dir) = exe.as_deref().and_then(Path::parent) {
        out.push(dir.join(&name));
        if cfg!(target_os = "macos") {
            out.push(dir.join("../Frameworks").join(&name));
        }
    }
    if cfg!(debug_assertions)
        && let Some(triple) = DEV_TRIPLE
    {
        out.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target-tailnet").join(triple).join(&name));
    }
    out
}

static GLOBAL: OnceLock<Result<&'static Tailnet, LoadError>> = OnceLock::new();

impl Tailnet {
    /// 进程里那一份库：第一次调用时按 [`candidates`] 找并加载，之后返回同一个结果
    /// （包括失败：换库要重启客户端）
    pub fn global() -> Result<&'static Tailnet, LoadError> {
        GLOBAL.get_or_init(Self::find).clone()
    }

    fn find() -> Result<&'static Tailnet, LoadError> {
        let list = candidates();
        // 绝对 / 相对路径先看文件在不在，免得把「没打进来」报成加载失败；安卓的裸文件名
        // 只能真去 dlopen 才知道
        let bare = |p: &Path| p.parent().is_none_or(|d| d.as_os_str().is_empty());
        for p in &list {
            if p.is_file() {
                return Self::open(p);
            }
            if bare(p) {
                match Self::open(p) {
                    Err(LoadError::Load { .. }) => continue,
                    other => return other,
                }
            }
        }
        Err(LoadError::NotFound { searched: list })
    }

    /// 加载指定的库文件并核对接口版本。库一经 dlopen 就不再卸载，失败也一样
    pub fn open(path: &Path) -> Result<&'static Tailnet, LoadError> {
        let load_err = |e: libloading::Error| LoadError::Load { path: path.to_path_buf(), message: e.to_string() };
        // SAFETY: dlopen 会执行库的初始化代码（Go 运行时在这里起来）。库是我们自己编的
        // libfalcon_tailnet，初始化不做别的事
        let lib = unsafe { libloading::Library::new(path) }.map_err(load_err)?;
        // Go 运行时已经在跑，dlclose 不安全：从这里起不论成败都不卸载
        let lib: &'static libloading::Library = Box::leak(Box::new(lib));
        // SAFETY: 符号类型与 main.go 的 //export 签名一致；函数指针在库的整个生命周期（进程
        // 结束前）都有效，因为库永不卸载
        unsafe {
            let abi: AbiVersionFn = *lib.get(b"ftn_abi_version\0").map_err(load_err)?;
            let found = abi();
            if found != ABI_VERSION {
                return Err(LoadError::AbiMismatch { path: path.to_path_buf(), found, expected: ABI_VERSION });
            }
            let t = Tailnet {
                path: path.to_path_buf(),
                start: *lib.get(b"ftn_start\0").map_err(load_err)?,
                status: *lib.get(b"ftn_status\0").map_err(load_err)?,
                proxy: *lib.get(b"ftn_proxy\0").map_err(load_err)?,
                stop: *lib.get(b"ftn_stop\0").map_err(load_err)?,
                logout: *lib.get(b"ftn_logout\0").map_err(load_err)?,
                set_interfaces: *lib.get(b"ftn_set_interfaces\0").map_err(load_err)?,
                free: *lib.get(b"ftn_free\0").map_err(load_err)?,
            };
            Ok(Box::leak(Box::new(t)))
        }
    }

    /// 实际加载的库文件
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 起节点；本进程已有节点时先停掉旧的再起（重启）。不等登录，进度看 [`Tailnet::status`]
    pub fn start(&self, cfg: &StartConfig) -> Result<(), Error> {
        let js = serde_json::to_string(cfg)?;
        // SAFETY: 签名见 main.go 的 ftn_start
        self.call_in(self.start, &js)
    }

    /// 当前状态。总有结果；JSON 解析不了说明库与客户端不配套
    pub fn status(&self) -> Result<Status, Error> {
        // SAFETY: 签名见 main.go 的 ftn_status
        match unsafe { self.take((self.status)()) } {
            Some(js) => Ok(serde_json::from_str(&js)?),
            None => Ok(Status::stopped()),
        }
    }

    /// 回环 SOCKS5 代理；节点不在 running 时是 `None`
    pub fn proxy(&self) -> Result<Option<ProxyInfo>, Error> {
        // SAFETY: 签名见 main.go 的 ftn_proxy
        match unsafe { self.take((self.proxy)()) } {
            Some(js) => Ok(Some(serde_json::from_str(&js)?)),
            None => Ok(None),
        }
    }

    /// 停掉节点，保留身份（下次 start 不用重新登录）
    pub fn stop(&self) {
        // SAFETY: 签名见 main.go 的 ftn_stop
        unsafe { (self.stop)() }
    }

    /// 向控制面退出登录、停掉节点，并清空状态目录。本机身份总会清掉；控制面那边退出失败
    /// （比如离线）时返回错误，这台设备会留在 tailnet 后台，要用户自己去删
    pub fn logout(&self, state_dir: &Path) -> Result<(), Error> {
        // SAFETY: 签名见 main.go 的 ftn_logout
        let res = unsafe { self.take((self.logout)()) };
        if state_dir.exists() {
            std::fs::remove_dir_all(state_dir)?;
        }
        match res {
            Some(js) => Err(go_error(&js)),
            None => Ok(()),
        }
    }

    /// 仅安卓：把 ConnectivityManager 读到的网卡与 DNS 交给节点。网络变了再调一次
    pub fn set_interfaces(&self, list: &InterfaceList) -> Result<(), Error> {
        let js = serde_json::to_string(list)?;
        self.call_in(self.set_interfaces, &js)
    }

    /// 调一个「JSON 进、NULL 或 {"error"} 出」的导出函数
    fn call_in(&self, f: StrInFn, js: &str) -> Result<(), Error> {
        let arg = CString::new(js).map_err(|e| Error::Invalid(e.to_string()))?;
        // SAFETY: arg 在调用期间有效；Go 侧只读不留指针（C.GoString 复制了一份）
        match unsafe { self.take(f(arg.as_ptr())) } {
            Some(js) => Err(go_error(&js)),
            None => Ok(()),
        }
    }

    /// 取走 Go 返回的字符串并交还 ftn_free
    ///
    /// SAFETY: `p` 必须是这份库某个导出函数刚返回的指针（NULL 或 C.CString 出来的）
    unsafe fn take(&self, p: *mut c_char) -> Option<String> {
        if p.is_null() {
            return None;
        }
        // SAFETY: 见函数约定；C.CString 以 NUL 结尾
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        // SAFETY: 同一份库 malloc 出来的，用它自己的 free 释放
        unsafe { (self.free)(p) };
        Some(s)
    }
}

fn go_error(js: &str) -> Error {
    match serde_json::from_str::<GoError>(js) {
        Ok(e) => Error::Node(e.error),
        Err(_) => Error::Node(js.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins() {
        let got = candidates_from(Some("/opt/x/lib.so".into()), Some("/app/bin/falcon".into()));
        assert_eq!(got, vec![PathBuf::from("/opt/x/lib.so")]);
        // 空值等于没设
        let got = candidates_from(Some("".into()), Some("/app/bin/falcon".into()));
        assert!(got.len() > 1 || cfg!(not(debug_assertions)));
    }

    #[test]
    fn looks_next_to_executable_first() {
        let name = PathBuf::from(libloading::library_filename(LIB_STEM));
        let got = candidates_from(None, Some(PathBuf::from("/app/Contents/MacOS/falcon-desktop")));
        let exe_dir = PathBuf::from("/app/Contents/MacOS").join(&name);
        assert!(got.contains(&exe_dir));
        if cfg!(target_os = "macos") {
            let fw = PathBuf::from("/app/Contents/MacOS/../Frameworks").join(&name);
            let (a, b) = (got.iter().position(|p| *p == exe_dir), got.iter().position(|p| *p == fw));
            assert!(a < b, "{got:?}");
        }
        if cfg!(debug_assertions) && DEV_TRIPLE.is_some() {
            assert!(got.last().unwrap().to_string_lossy().contains("target-tailnet"));
        }
    }

    #[test]
    fn go_error_falls_back_to_raw_text() {
        assert_eq!(go_error(r#"{"error":"boom"}"#).to_string(), "boom");
        assert_eq!(go_error("garbage").to_string(), "garbage");
    }
}
