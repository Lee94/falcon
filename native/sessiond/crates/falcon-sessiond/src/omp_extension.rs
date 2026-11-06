//! Installs the bundled OMP display extension without changing OMP configuration.

#[cfg(unix)]
const EXTENSION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../integrations/omp/falcon-tern.ts"
));

/// Make the bundled extension available to the target OMP profile on its next start.
///
/// `Some(argv)` must be the verified foreground process's argv, including argv[0].
/// Direct OMP launches and its published Bun entry point are recognizable; shell
/// wrappers cannot reveal their eventual profile without inspecting other processes.
/// `None` installs for future launches using this helper's public environment.
/// Directory overrides always come from that environment, which may differ from
/// changes made inside the target shell; no target process environment is read.
pub fn install_for(argv: Option<&[String]>) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        unix::install(argv)
    }
    #[cfg(not(unix))]
    {
        let _ = argv;
        anyhow::bail!("installing the Falcon Tern OMP extension is only supported on Unix")
    }
}

#[cfg(unix)]
pub fn recognizes(argv: &[String]) -> bool {
    unix::omp_args(argv).is_some()
}

#[cfg(unix)]
mod unix {
    use std::env;
    use std::ffi::{CStr, CString, OsString};
    use std::fs::{self, DirBuilder, File, OpenOptions};
    use std::io::{self, Read, Seek, SeekFrom, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use anyhow::{Context, bail};

    const FILE_NAME: &CStr = c"falcon-tern.ts";
    const OWNER_LINE: &[u8] = b"// Falcon Tern display bridge.";
    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    pub(super) fn install(argv: Option<&[String]>) -> anyhow::Result<()> {
        let agent_dir = select_agent_dir(argv, |name| env::var_os(name))?;
        install_at(&agent_dir)
            .with_context(|| format!("installing the Falcon Tern OMP extension in {}", agent_dir.display()))
    }

    // OMP v18.16.0: docs/extension-loading.md, docs/config-usage.md and
    // packages/utils/src/dirs.ts. Last --profile wins over OMP_PROFILE, then
    // PI_PROFILE; a defined empty OMP_PROFILE suppresses PI_PROFILE. Trimmed
    // empty/default selects the default profile. Named profiles ignore the
    // agent override and use <HOME>/<PI_CONFIG_DIR or .omp>/profiles/<name>/agent.
    // Default uses PI_CODING_AGENT_DIR, otherwise <HOME>/<config root>/agent.
    // XDG only relocates data/state/cache, never this extension config path.
    fn select_agent_dir(argv: Option<&[String]>, env: impl Fn(&str) -> Option<OsString>) -> anyhow::Result<PathBuf> {
        let cli_profile = profile_flag(argv)?;
        let omp_profile = env("OMP_PROFILE");
        let pi_profile = env("PI_PROFILE");
        let selected = cli_profile.map(std::ffi::OsStr::new).or(omp_profile.as_deref()).or(pi_profile.as_deref());
        let profile = normalize_profile(selected)?;
        let agent_override = env("PI_CODING_AGENT_DIR").filter(|value| !value.is_empty());
        let home = env("HOME").filter(|value| !value.is_empty());
        let config = env("PI_CONFIG_DIR").filter(|value| !value.is_empty()).unwrap_or_else(|| ".omp".into());
        let config_root = || -> anyhow::Result<PathBuf> {
            let home = home.as_ref().context("HOME is unset or empty; cannot locate the OMP agent directory")?;
            // Node path.join(home, config) keeps even a leading '/' in config
            // under home (unlike Rust Path::join). Do not expand '~' here.
            use std::os::unix::ffi::OsStrExt;
            let suffix = config.as_bytes();
            let start = suffix.iter().position(|byte| *byte != b'/').unwrap_or(suffix.len());
            Ok(normalize_path(&PathBuf::from(home).join(std::ffi::OsStr::from_bytes(&suffix[start..]))))
        };
        if let Some(profile) = profile {
            return Ok(config_root()?.join("profiles").join(profile).join("agent"));
        }
        if let Some(agent) = agent_override {
            // setProfile propagates a derived PI_CODING_AGENT_DIR to children.
            // Explicitly selecting default must not mistake that for a custom
            // default override, including when empty OMP_PROFILE shadows PI_PROFILE.
            let inherited = normalize_profile(omp_profile.as_deref().or(pi_profile.as_deref()))
                .ok().flatten()
                .or_else(|| normalize_profile(pi_profile.as_deref()).ok().flatten());
            let derived = inherited.map(|name| config_root().map(|root| root.join("profiles").join(name).join("agent"))).transpose()?;
            if derived.as_deref() != Some(Path::new(&agent)) {
                let agent = PathBuf::from(agent);
                if !agent.is_absolute() {
                    bail!("relative PI_CODING_AGENT_DIR depends on the target OMP working directory; use an absolute directory in the helper environment");
                }
                return Ok(normalize_path(&agent));
            }
        }
        Ok(config_root()?.join("agent"))
    }

    fn normalize_profile(profile: Option<&std::ffi::OsStr>) -> anyhow::Result<Option<&str>> {
        let Some(profile) = profile else { return Ok(None) };
        let name = profile.to_str().context("OMP profile is not valid UTF-8")?.trim();
        if name.is_empty() || name == "default" {
            return Ok(None);
        }
        let bytes = name.as_bytes();
        let basename = name.split('.').next().unwrap_or_default();
        let reserved = matches!(basename, "con" | "prn" | "aux" | "nul")
            || (basename.len() == 4 && (basename.starts_with("com") || basename.starts_with("lpt"))
                && basename.as_bytes()[3].is_ascii_digit());
        if bytes.len() > 64 || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit()
            || !bytes.iter().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
            || name.ends_with('.') || reserved
        {
            bail!("invalid OMP profile: expected 1–64 lowercase letters, digits, '.', '_' or '-', starting with a letter/digit, without a trailing dot or reserved device name");
        }
        Ok(Some(name))
    }

    fn normalize_path(path: &Path) -> PathBuf {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::ParentDir => { normalized.pop(); }
                Component::CurDir => {}
                component => normalized.push(component.as_os_str()),
            }
        }
        normalized
    }

    pub(super) fn omp_args(argv: &[String]) -> Option<&[String]> {
        let executable = Path::new(argv.first()?).file_name()?.to_str()?;
        match executable {
            "omp" => Some(&argv[1..]),
            "bun" | "node" if argv.get(1).is_some_and(|entry| {
                ["@oh-my-pi/pi-coding-agent/src/cli.ts", "@oh-my-pi/pi-coding-agent/dist/cli.js",
                    "packages/coding-agent/src/cli.ts", "packages/coding-agent/dist/cli.js"]
                    .iter().any(|suffix| Path::new(entry).ends_with(suffix))
            }) => Some(&argv[2..]),
            _ => None,
        }
    }

    fn profile_flag(argv: Option<&[String]>) -> anyhow::Result<Option<&str>> {
        let Some(argv) = argv else { return Ok(None) };
        let args = omp_args(argv).context("target argv is not a recognizable OMP launch; a shell/script wrapper's profile cannot be inferred (launch omp directly)")?;
        // Match v18.16.0 cli/profile-bootstrap.ts value consumption: a
        // --profile token used as another option's value is never a selector.
        const STRING_FLAGS: &[&str] = &[
            "--cwd", "--config", "--add-dir", "--mode", "--fork", "--provider", "--model", "--smol", "--slow",
            "--goal", "--prewalk-into", "--plan-yolo-into", "--max-time", "--warn-cost", "--max-cost",
            "--service-tier", "--api-key", "--system-prompt", "--system-prompt-template", "--append-system-prompt",
            "--provider-session-id", "--prompt-cache-key", "--session-dir", "--models", "--tools", "--thinking",
            "--export", "--hook", "--extension", "-e", "--trusted-extension", "--plugin-dir", "--skills", "--approval-mode",
        ];
        const VALUELESS_FLAGS: &[&str] = &[
            "--help", "--version", "--allow-home", "--continue", "--from-claude", "--from-codex", "--no-session",
            "--no-tools", "--no-lsp", "--no-pty", "--hide-thinking", "--advisor", "--wait-async", "--external-thinking",
            "--prewalk", "--no-prewalk", "--plan-yolo", "--print", "--print-thoughts", "--no-extensions", "--no-skills",
            "--no-rules", "--no-context-files", "--no-title", "--no-ui", "--auto-approve", "--yolo",
        ];
        const COMMANDS: &[&str] = &[
            "auth", "auth-broker", "auth-gateway", "agents", "anonymize", "bench", "browser-relay", "cleanse", "collab",
            "commit", "completions", "__complete", "compress", "config", "dry-balance", "find", "gc", "grep", "gallery",
            "git", "grievances", "images", "img", "if-bench", "install", "join", "login", "models", "plugin", "plugins",
            "predict", "ps", "say", "clip", "play", "share", "session", "setup", "shell", "read", "render", "skill", "skills",
            "ssh", "stats", "stream", "update", "usage", "tiny-models", "token", "toks", "ttsr", "worktree", "wt",
            "search", "q", "web-search",
        ];
        let mut profile = None;
        let mut index = 0;
        let mut can_dispatch = true;
        while let Some(arg) = args.get(index).map(String::as_str) {
            if arg == "--" {
                break;
            }
            if arg == "--profile" || arg == "--alias" {
                let value = args.get(index + 1).filter(|value| !value.is_empty() && !value.starts_with('-'))
                    .with_context(|| format!("{arg} requires a value in target OMP argv"))?;
                if arg == "--profile" { profile = Some(value.as_str()); }
                index += 2;
                continue;
            }
            if let Some(value) = arg.strip_prefix("--profile=").or_else(|| arg.strip_prefix("--alias=")) {
                if value.is_empty() { bail!("OMP global profile/alias flag requires a value"); }
                if arg.starts_with("--profile=") { profile = Some(value); }
                index += 1;
                continue;
            }
            if can_dispatch && COMMANDS.contains(&arg) {
                break;
            }
            can_dispatch = false;
            let optional = matches!(arg, "--plan" | "--resume" | "-r" | "--restore" | "--session");
            let unknown = arg.starts_with("--") && !arg.contains('=') && !VALUELESS_FLAGS.contains(&arg);
            let consumes = STRING_FLAGS.contains(&arg)
                || ((optional || unknown) && args.get(index + 1).is_some_and(|value| !value.starts_with('-')));
            index += if consumes { 2 } else { 1 };
        }
        Ok(profile)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn directory(argv: Option<&[&str]>, variables: &[(&str, &str)]) -> anyhow::Result<PathBuf> {
            let argv = argv.map(|args| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
            select_agent_dir(argv.as_deref(), |key| {
                variables.iter().find(|(name, _)| *name == key).map(|(_, value)| OsString::from(*value))
                    .or_else(|| (key == "HOME").then(|| OsString::from("/home/test")))
            })
        }

        #[test]
        fn default_and_named_profile_directories() {
            for (variables, expected) in [
                (vec![], "/home/test/.omp/agent"),
                (vec![("OMP_PROFILE", "work")], "/home/test/.omp/profiles/work/agent"),
                (vec![("PI_PROFILE", "legacy")], "/home/test/.omp/profiles/legacy/agent"),
                (vec![("OMP_PROFILE", " work ")], "/home/test/.omp/profiles/work/agent"),
                (vec![("OMP_PROFILE", "default")], "/home/test/.omp/agent"),
                (vec![("OMP_PROFILE", " \t ")], "/home/test/.omp/agent"),
                (vec![("PI_CONFIG_DIR", "config/omp")], "/home/test/config/omp/agent"),
                (vec![("PI_CONFIG_DIR", "")], "/home/test/.omp/agent"),
                (vec![("PI_CONFIG_DIR", "/custom"), ("OMP_PROFILE", "work")], "/home/test/custom/profiles/work/agent"),
                (vec![("PI_CONFIG_DIR", ".omp/../omp")], "/home/test/omp/agent"),
                (vec![("OMP_PROFILE", "work"), ("XDG_DATA_HOME", "/data"), ("XDG_STATE_HOME", "/state"),
                    ("XDG_CACHE_HOME", "/cache")], "/home/test/.omp/profiles/work/agent"),
            ] {
                assert_eq!(directory(None, &variables).unwrap(), Path::new(expected));
            }
        }

        #[test]
        fn cli_then_canonical_then_legacy_profile_precedence() {
            let variables = [("OMP_PROFILE", "canonical"), ("PI_PROFILE", "legacy")];
            assert_eq!(directory(None, &variables).unwrap(), Path::new("/home/test/.omp/profiles/canonical/agent"));
            assert_eq!(directory(Some(&["omp", "--profile", "cli"]), &variables).unwrap(),
                Path::new("/home/test/.omp/profiles/cli/agent"));
            assert_eq!(directory(Some(&["omp", "--profile", "first", "--profile=last"]), &variables).unwrap(),
                Path::new("/home/test/.omp/profiles/last/agent"));
            for default in ["", "default", " \t"] {
                assert_eq!(directory(None, &[("OMP_PROFILE", default), ("PI_PROFILE", "legacy")]).unwrap(),
                    Path::new("/home/test/.omp/agent"));
            }
            assert_eq!(directory(Some(&["omp", "--profile=default"]), &variables).unwrap(),
                Path::new("/home/test/.omp/agent"));
            // An explicit CLI selector also supersedes an invalid environment selector.
            assert_eq!(directory(Some(&["omp", "--profile=cli"]), &[("OMP_PROFILE", "../bad")]).unwrap(),
                Path::new("/home/test/.omp/profiles/cli/agent"));
        }

        #[test]
        fn agent_override_only_applies_to_default_profile() {
            let variables = [("PI_CODING_AGENT_DIR", "/custom/agent"), ("PI_CONFIG_DIR", ".different")];
            assert_eq!(directory(None, &variables).unwrap(), Path::new("/custom/agent"));
            assert_eq!(directory(Some(&["omp", "--profile=work"]), &variables).unwrap(),
                Path::new("/home/test/.different/profiles/work/agent"));
            assert_eq!(directory(None, &[("PI_CODING_AGENT_DIR", "")]).unwrap(), Path::new("/home/test/.omp/agent"));
            assert!(directory(None, &[("PI_CODING_AGENT_DIR", "relative/agent")]).is_err());
            assert_eq!(directory(Some(&["omp", "--profile=work"]), &[("PI_CODING_AGENT_DIR", "relative/agent")]).unwrap(),
                Path::new("/home/test/.omp/profiles/work/agent"));
            // A direct default override needs no home directory.
            assert_eq!(select_agent_dir(None, |key| (key == "PI_CODING_AGENT_DIR").then(|| "/custom/agent".into())).unwrap(),
                Path::new("/custom/agent"));
        }

        #[test]
        fn inherited_profile_directory_does_not_become_default_override() {
            let variables = [("OMP_PROFILE", "work"), ("PI_CODING_AGENT_DIR", "/home/test/.omp/profiles/work/agent")];
            assert_eq!(directory(Some(&["omp", "--profile=default"]), &variables).unwrap(), Path::new("/home/test/.omp/agent"));
            assert_eq!(directory(None, &[("OMP_PROFILE", ""), ("PI_PROFILE", "work"),
                ("PI_CODING_AGENT_DIR", "/home/test/.omp/profiles/work/agent")]).unwrap(), Path::new("/home/test/.omp/agent"));
        }

        #[test]
        fn profile_tokens_follow_bootstrap_value_boundaries() {
            for args in [
                vec!["omp", "--", "--profile", "literal"],
                vec!["omp", "--system-prompt", "--profile", "literal"],
                vec!["omp", "--extension", "--profile", "literal"],
                vec!["omp", "grep", "--profile", "literal"],
            ] {
                assert_eq!(directory(Some(&args), &[]).unwrap(), Path::new("/home/test/.omp/agent"));
            }
            for args in [
                vec!["omp", "launch", "--profile", "work"],
                vec!["omp", "acp", "--profile", "work"],
                vec!["omp", "--resume", "--profile", "work"],
                vec!["omp", "--plan", "--profile", "work"],
                vec!["omp", "--print", "--profile", "work"],
                vec!["omp", "--unknown", "--profile", "work"],
                vec!["omp", "--unknown", "value", "--profile", "work"],
                vec!["omp", "prompt", "grep", "--profile", "work"],
                vec!["omp", "--alias", "shortcut", "--profile", "work"],
                vec!["/bin/bun", "/node_modules/@oh-my-pi/pi-coding-agent/src/cli.ts", "--profile=work"],
            ] {
                assert_eq!(directory(Some(&args), &[]).unwrap(), Path::new("/home/test/.omp/profiles/work/agent"));
            }
        }

        #[test]
        fn invalid_profiles_and_unrecognizable_wrappers_fail_without_fallback() {
            for name in ["../escape", "Upper", "foo/bar", "foo.", "con", "nul.txt", "com0", "lpt9", ".hidden",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
                assert!(directory(None, &[("OMP_PROFILE", name)]).is_err(), "{name}");
            }
            for args in [vec![], vec!["omp", "--profile"], vec!["omp", "--profile="],
                vec!["sh", "-c", "omp --profile work"], vec!["wrapper", "--profile", "work"],
                vec!["bun", "unknown.ts", "--profile", "work"]] {
                assert!(directory(Some(&args), &[]).is_err());
            }
            assert!(select_agent_dir(None, |_| None).is_err());
        }
    }

    fn install_at(agent_dir: &Path) -> anyhow::Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(agent_dir)
            .with_context(|| format!("creating OMP agent directory {}", agent_dir.display()))?;
        let agent = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(agent_dir)
            .with_context(|| format!("opening OMP agent directory {}", agent_dir.display()))?;
        check_metadata(&agent.metadata()?, true)?;

        // Keep all extension operations relative to an open directory, so a rename
        // of the configured path cannot redirect the installation elsewhere.
        let created = unsafe { libc::mkdirat(agent.as_raw_fd(), c"extensions".as_ptr(), 0o700) };
        if created == -1 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error).context("creating the OMP extensions directory");
            }
        }
        let directory = open_at(
            &agent,
            c"extensions",
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )
        .context("opening the OMP extensions directory")?;
        check_metadata(&directory.metadata()?, true)?;

        match open_at(
            &directory,
            FILE_NAME,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        ) {
            Ok(mut existing) => {
                let metadata = existing.metadata()?;
                check_metadata(&metadata, false)?;
                if !has_owner_line(&mut existing)? {
                    bail!("refusing to replace falcon-tern.ts: its first line is not the Falcon Tern ownership marker");
                }
                if metadata.len() == super::EXTENSION.len() as u64 {
                    existing.seek(SeekFrom::Start(0))?;
                    if matches_embedded(&mut existing)? {
                        return Ok(());
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("opening existing falcon-tern.ts without following symlinks"),
        }

        let (mut file, temporary) = create_temporary(&directory)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .context("setting extension file permissions to 0600")?;
        file.write_all(super::EXTENSION.as_bytes())
            .context("writing the bundled OMP extension")?;
        file.sync_all().context("syncing the bundled OMP extension")?;
        let renamed = unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary.name.as_ptr(),
                directory.as_raw_fd(),
                FILE_NAME.as_ptr(),
            )
        };
        if renamed == -1 {
            return Err(io::Error::last_os_error()).context("atomically installing falcon-tern.ts");
        }
        Ok(())
    }

    fn check_metadata(metadata: &fs::Metadata, directory: bool) -> anyhow::Result<()> {
        if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
            bail!("OMP extension installation target has an unexpected file type");
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("refusing to use an OMP extension installation target owned by another user");
        }
        if metadata.mode() & 0o022 != 0 {
            bail!("refusing to use an OMP extension installation target writable by group or others");
        }
        Ok(())
    }

    fn has_owner_line(file: &mut File) -> io::Result<bool> {
        let mut marker = [0; OWNER_LINE.len()];
        match file.read_exact(&mut marker) {
            Ok(()) if marker == OWNER_LINE => {}
            Ok(()) => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(error) => return Err(error),
        }
        let mut ending = [0];
        match file.read_exact(&mut ending) {
            Ok(()) if ending[0] == b'\n' => Ok(true),
            Ok(()) if ending[0] == b'\r' => match file.read_exact(&mut ending) {
                Ok(()) => Ok(ending[0] == b'\n'),
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
                Err(error) => Err(error),
            },
            Ok(()) => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(true),
            Err(error) => Err(error),
        }
    }

    fn matches_embedded(file: &mut File) -> io::Result<bool> {
        let mut remaining = super::EXTENSION.as_bytes();
        let mut buffer = [0; 8192];
        loop {
            let count = match file.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                return Ok(remaining.is_empty());
            }
            if !remaining.starts_with(&buffer[..count]) {
                return Ok(false);
            }
            remaining = &remaining[count..];
        }
    }

    fn open_at(directory: &File, name: &CStr, flags: libc::c_int, mode: libc::mode_t) -> io::Result<File> {
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, mode as libc::c_uint) };
        if descriptor == -1 {
            Err(io::Error::last_os_error())
        } else {
            // The descriptor is newly opened and has exactly one owner.
            Ok(unsafe { File::from_raw_fd(descriptor) })
        }
    }

    struct Temporary<'a> {
        directory: &'a File,
        name: CString,
    }

    impl Drop for Temporary<'_> {
        fn drop(&mut self) {
            // Also runs on write/sync/rename failures. After a successful rename,
            // this name no longer exists and unlinkat is harmless.
            unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
        }
    }

    fn create_temporary(directory: &File) -> io::Result<(File, Temporary<'_>)> {
        loop {
            let serial = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let name = CString::new(format!(".falcon-tern.{}.{serial}.tmp", std::process::id()))
                .expect("temporary extension name contains no NUL");
            match open_at(
                directory,
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            ) {
                Ok(file) => return Ok((file, Temporary { directory, name })),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}
