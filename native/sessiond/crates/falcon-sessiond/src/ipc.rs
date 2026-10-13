//! holder 的监听端点与连接：Unix 是 socket，Windows 是命名管道（设计文档第 3 节）。
//!
//! 都是阻塞 I/O：holder 每个连接一读一写两条线程，连接数是个位数，不值得为它上异步运行时。
//!
//! 权限：Unix 上 socket 0600、目录 0700，accept 后再核对对端 uid；Windows 上管道的 DACL 只给
//! 当前用户，并带 `PIPE_REJECT_REMOTE_CLIENTS`。

use std::io::{Read, Write};

pub type Reader = Box<dyn Read + Send>;
pub type Writer = Box<dyn Write + Send>;

pub use imp::{Closer, Stream, connect, listen};

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::io::AsRawFd as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    use super::{Reader, Writer};

    pub struct Listener {
        inner: UnixListener,
    }

    pub struct Stream(UnixStream);

    fn uid() -> u32 {
        unsafe { libc::getuid() }
    }

    /// 超长路径时落到 /tmp 下的私有目录：必须是自己的、0700，否则拒绝（别人预先建好的目录
    /// 可以拿来劫持 socket）
    fn ensure_private_parent(path: &Path) -> io::Result<()> {
        let Some(dir) = path.parent() else { return Ok(()) };
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let meta = std::fs::metadata(dir)?;
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != uid() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} 不归当前用户所有", dir.display())));
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    pub fn listen(endpoint: &str) -> io::Result<Listener> {
        let path = PathBuf::from(endpoint);
        ensure_private_parent(&path)?;
        if path.exists() {
            // 连得上说明有人在用（同一个会话的另一个 holder？），不能抢；连不上是上次留下的残骸
            if UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, format!("{endpoint} 已经有人在监听")));
            }
            std::fs::remove_file(&path)?;
        }
        let inner = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Listener { inner })
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn peer_uid(s: &UnixStream) -> Option<u32> {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let r = unsafe {
            libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len)
        };
        (r == 0).then_some(cred.uid)
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn peer_uid(s: &UnixStream) -> Option<u32> {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        let r = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
        (r == 0).then_some(uid)
    }

    impl Listener {
        pub fn accept(&self) -> io::Result<Stream> {
            loop {
                let (s, _) = self.inner.accept()?;
                if peer_uid(&s) == Some(uid()) {
                    return Ok(Stream(s));
                }
                // 不是自己人：直接断开，接着等
            }
        }
    }

    impl Stream {
        pub fn split(self) -> io::Result<(Reader, Writer)> {
            let w = self.0.try_clone()?;
            Ok((Box::new(self.0), Box::new(HalfCloseWriter(w))))
        }

        /// 从外面强行断开（读线程阻塞在 read 上也能叫醒）
        pub fn closer(&self) -> io::Result<Closer> {
            Ok(Closer(self.0.try_clone()?))
        }
    }

    pub struct Closer(UnixStream);

    impl Closer {
        pub fn close(&self) {
            let _ = self.0.shutdown(std::net::Shutdown::Both);
        }
    }

    /// 写端丢掉时只关写方向：对端读到 EOF，而读端还能把对端最后的话读完
    struct HalfCloseWriter(UnixStream);

    impl std::io::Write for HalfCloseWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    impl Drop for HalfCloseWriter {
        fn drop(&mut self) {
            let _ = self.0.shutdown(std::net::Shutdown::Write);
        }
    }

    pub fn connect(endpoint: &str) -> io::Result<Stream> {
        UnixStream::connect(endpoint).map(Stream)
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::sync::Arc;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
        GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
    };
    use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken};

    use super::{Reader, Writer};

    struct Handle(HANDLE);

    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_err() -> io::Error {
        io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
    }

    /// 只给当前用户的安全描述符（`D:P(A;;GA;;;<用户 SID>)`）。拿不到 SID 时报错，
    /// 不退回默认 DACL——默认 DACL 会放行同机的管理员与 SYSTEM 之外的人也说不准
    struct Security {
        sd: PSECURITY_DESCRIPTOR,
        attrs: SECURITY_ATTRIBUTES,
    }

    unsafe impl Send for Security {}
    unsafe impl Sync for Security {}

    impl Drop for Security {
        fn drop(&mut self) {
            unsafe { LocalFree(self.sd as _) };
        }
    }

    fn user_sid_string() -> io::Result<String> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(last_err());
            }
            let token = Handle(token);
            let mut len = 0u32;
            GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut buf = vec![0u8; len as usize];
            if GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
                return Err(last_err());
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
                return Err(last_err());
            }
            let n = (0..).take_while(|&i| *s.add(i) != 0).count();
            let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s as _);
            Ok(out)
        }
    }

    fn security() -> io::Result<Security> {
        let sddl = wide(&format!("D:P(A;;GA;;;{})", user_sid_string()?));
        unsafe {
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) == 0 {
                return Err(last_err());
            }
            Ok(Security {
                sd,
                attrs: SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: sd,
                    bInheritHandle: 0,
                },
            })
        }
    }

    pub struct Listener {
        name: Vec<u16>,
        sec: Security,
        next: std::sync::Mutex<Option<Handle>>,
    }

    impl Listener {
        fn instance(&self, first: bool) -> io::Result<Handle> {
            let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
            if first {
                // 已有同名管道（同一会话的另一个 holder）时直接失败，不和它抢
                open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
            }
            let h = unsafe {
                CreateNamedPipeW(
                    self.name.as_ptr(),
                    open_mode,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    64 * 1024,
                    64 * 1024,
                    0,
                    &self.sec.attrs,
                )
            };
            if h == INVALID_HANDLE_VALUE {
                return Err(last_err());
            }
            Ok(Handle(h))
        }

        pub fn accept(&self) -> io::Result<Stream> {
            let h = match self.next.lock().unwrap_or_else(|e| e.into_inner()).take() {
                Some(h) => h,
                None => self.instance(false)?,
            };
            unsafe {
                let event = Handle(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()));
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = event.0;
                if ConnectNamedPipe(h.0, &mut ov) == 0 {
                    match GetLastError() {
                        ERROR_PIPE_CONNECTED => {}
                        ERROR_IO_PENDING => {
                            let mut n = 0u32;
                            if GetOverlappedResult(h.0, &ov, &mut n, 1) == 0 {
                                return Err(last_err());
                            }
                        }
                        e => return Err(io::Error::from_raw_os_error(e as i32)),
                    }
                }
            }
            // 下一个实例先建好：客户端在两次 accept 之间连进来时不至于找不到管道
            *self.next.lock().unwrap_or_else(|e| e.into_inner()) = self.instance(false).ok();
            Ok(Stream(Arc::new(h)))
        }
    }

    pub fn listen(endpoint: &str) -> io::Result<Listener> {
        let l = Listener { name: wide(endpoint), sec: security()?, next: std::sync::Mutex::new(None) };
        let first = l.instance(true)?;
        *l.next.lock().unwrap_or_else(|e| e.into_inner()) = Some(first);
        Ok(l)
    }

    /// 管道句柄。同步句柄上的 I/O 会被系统串行化（一条线程阻塞在 ReadFile 上时，另一条线程的
    /// WriteFile 也得等），所以一律用重叠 I/O，每次操作自带一个事件
    #[derive(Clone)]
    pub struct Stream(Arc<Handle>);

    fn io(h: HANDLE, buf: *mut u8, len: usize, write: bool) -> io::Result<usize> {
        unsafe {
            let event = Handle(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()));
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = event.0;
            let mut n = 0u32;
            let len = len.min(u32::MAX as usize) as u32;
            let ok = if write {
                WriteFile(h, buf as *const u8, len, &mut n, &mut ov)
            } else {
                ReadFile(h, buf, len, &mut n, &mut ov)
            };
            if ok == 0 {
                match GetLastError() {
                    ERROR_IO_PENDING => {
                        if GetOverlappedResult(h, &ov, &mut n, 1) == 0 {
                            return match GetLastError() {
                                ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED if !write => Ok(0),
                                e => Err(io::Error::from_raw_os_error(e as i32)),
                            };
                        }
                    }
                    ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED if !write => return Ok(0),
                    e => return Err(io::Error::from_raw_os_error(e as i32)),
                }
            }
            Ok(n as usize)
        }
    }

    impl io::Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            io(self.0.0, buf.as_mut_ptr(), buf.len(), false)
        }
    }

    impl io::Write for Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            io(self.0.0, buf.as_ptr() as *mut u8, buf.len(), true)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Stream {
        pub fn split(self) -> io::Result<(Reader, Writer)> {
            Ok((Box::new(self.clone()), Box::new(self)))
        }

        /// 从外面强行断开：取消挂着的重叠 I/O 并断开管道，阻塞在读上的线程会醒来读到 EOF
        pub fn closer(&self) -> io::Result<Closer> {
            Ok(Closer(self.0.clone()))
        }
    }

    pub struct Closer(Arc<Handle>);

    impl Closer {
        pub fn close(&self) {
            unsafe {
                windows_sys::Win32::System::IO::CancelIoEx(self.0.0, std::ptr::null());
                windows_sys::Win32::System::Pipes::DisconnectNamedPipe(self.0.0);
            }
        }
    }

    pub fn connect(endpoint: &str) -> io::Result<Stream> {
        let name = wide(endpoint);
        for _ in 0..20 {
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                return Ok(Stream(Arc::new(Handle(h))));
            }
            if unsafe { GetLastError() } != ERROR_PIPE_BUSY {
                return Err(last_err());
            }
            unsafe { WaitNamedPipeW(name.as_ptr(), 500) };
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "管道一直忙"))
    }
}
