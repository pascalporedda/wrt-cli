use anyhow::{Context, Result};
use std::env;
#[cfg(not(unix))]
use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::envx::ResolvedEnvironment;
use crate::gitx::Repo;
use crate::project::{CommandSpec, ProjectConfig};
use crate::state::{Allocation, State};

pub fn run_cmd(dir: &Path, cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("run {cmd}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!("command failed"));
    }
    Ok(())
}

pub fn run_argv_with_wrt_env(
    repo: &Repo,
    state: &State,
    dir: &Path,
    a: &Allocation,
    project: Option<&ProjectConfig>,
    argv: &[String],
) -> Result<i32> {
    let environment = ResolvedEnvironment::build(repo, state, a, project)?;
    run_argv_with_environment(state, dir, a, argv, &environment)
}

fn run_argv_with_environment(
    state: &State,
    dir: &Path,
    a: &Allocation,
    argv: &[String],
    environment: &ResolvedEnvironment,
) -> Result<i32> {
    let cmd = &argv[0];
    let cmd_args = &argv[1..];

    let mut c = Command::new(cmd);
    c.args(cmd_args).current_dir(dir);
    environment.apply_to(&mut c);
    if Path::new(cmd).file_name().and_then(|name| name.to_str()) == Some("supabase") {
        // Supabase CLI reads `.git/HEAD` to label local database commands. Linked Git
        // worktrees use a `.git` file, so its fallback can report the managed root's branch.
        // GITHUB_HEAD_REF is Supabase's first-choice branch signal.
        let branch = state
            .allocations
            .values()
            .filter(|allocation| dir.starts_with(Path::new(&allocation.path)))
            .max_by_key(|allocation| Path::new(&allocation.path).components().count())
            .map(|allocation| allocation.branch.as_str())
            .unwrap_or(&a.branch);
        c.env("GITHUB_HEAD_REF", branch);
    }

    let status = c.status().with_context(|| format!("run {cmd}"))?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }
    Ok(0)
}

pub fn run_project_command(
    state: &State,
    worktree_root: &Path,
    allocation: &Allocation,
    environment: &ResolvedEnvironment,
    command: &CommandSpec,
) -> Result<i32> {
    run_argv_with_environment(
        state,
        &command.working_dir(worktree_root)?,
        allocation,
        command.argv(),
        environment,
    )
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for p in env::split_paths(&path) {
        let cand = p.join(bin);
        if cand.exists() {
            return Some(cand);
        }
    }
    None
}

pub fn sh_quote(s: &str) -> String {
    let mut out = String::from("'");
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

pub fn atomic_write_private(trusted_root: &Path, path: &Path, contents: &[u8]) -> Result<()> {
    #[cfg(unix)]
    return unix_private_io::atomic_write(trusted_root, path, contents);

    #[cfg(not(unix))]
    {
        atomic_write_private_fallback(trusted_root, path, contents)
    }
}

#[cfg(not(unix))]
fn atomic_write_private_fallback(trusted_root: &Path, path: &Path, contents: &[u8]) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    validate_path_below_root(trusted_root, path, true)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("file has no parent: {}", path.display()))?;
    validate_write_target(trusted_root, path)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid file name: {}", path.display()))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(contents)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
        validate_path_below_root(trusted_root, path, false)?;
        validate_write_target(trusted_root, path)?;
        fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("sync {}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn atomic_copy_private(
    source_root: &Path,
    source: &Path,
    destination_root: &Path,
    destination: &Path,
) -> Result<()> {
    #[cfg(unix)]
    return unix_private_io::atomic_copy(source_root, source, destination_root, destination);

    #[cfg(not(unix))]
    {
        atomic_copy_private_fallback(source_root, source, destination_root, destination)
    }
}

#[cfg(not(unix))]
fn atomic_copy_private_fallback(
    source_root: &Path,
    source: &Path,
    destination_root: &Path,
    destination: &Path,
) -> Result<()> {
    use std::io::Read;

    validate_path_below_root(source_root, source, false)?;
    let mut file = File::open(source).with_context(|| format!("open {}", source.display()))?;
    let opened = file
        .metadata()
        .with_context(|| format!("inspect opened {}", source.display()))?;
    let named = fs::symlink_metadata(source)
        .with_context(|| format!("inspect named {}", source.display()))?;
    if named.file_type().is_symlink() || !named.is_file() || !opened.is_file() {
        anyhow::bail!(
            "refusing non-regular or symlink source: {}",
            source.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != named.dev() || opened.ino() != named.ino() {
            anyhow::bail!("copy source changed while opening: {}", source.display());
        }
    }
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .with_context(|| format!("read {}", source.display()))?;
    atomic_write_private(destination_root, destination, &contents)
}

#[cfg(unix)]
mod unix_private_io {
    use super::*;
    use std::ffi::{CString, OsStr};
    use std::io::{Read, Write};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::ffi::OsStrExt;

    pub fn atomic_write(trusted_root: &Path, path: &Path, contents: &[u8]) -> Result<()> {
        let (parent, file_name) = open_parent(trusted_root, path, true)?;
        validate_entry(parent.as_raw_fd(), &file_name, path)?;

        let temporary_name = CString::new(format!(".wrt.{}.tmp", uuid::Uuid::new_v4()))?;
        let temporary = openat_file(
            parent.as_raw_fd(),
            &temporary_name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
        .with_context(|| format!("create temporary file below {}", trusted_root.display()))?;

        let result = (|| {
            let mut temporary = temporary;
            temporary
                .write_all(contents)
                .with_context(|| format!("write private file {}", path.display()))?;
            temporary
                .sync_all()
                .with_context(|| format!("sync private file {}", path.display()))?;
            validate_entry(parent.as_raw_fd(), &file_name, path)?;
            let renamed = unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    temporary_name.as_ptr(),
                    parent.as_raw_fd(),
                    file_name.as_ptr(),
                )
            };
            if renamed != 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("replace {}", path.display()));
            }
            parent
                .sync_all()
                .with_context(|| format!("sync parent of {}", path.display()))
        })();
        if result.is_err() {
            unsafe {
                libc::unlinkat(parent.as_raw_fd(), temporary_name.as_ptr(), 0);
            }
        }
        result
    }

    pub fn atomic_copy(
        source_root: &Path,
        source: &Path,
        destination_root: &Path,
        destination: &Path,
    ) -> Result<()> {
        let (parent, file_name) = open_parent(source_root, source, false)?;
        let mut file = openat_file(
            parent.as_raw_fd(),
            &file_name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )
        .with_context(|| format!("open private copy source {}", source.display()))?;
        if !file
            .metadata()
            .with_context(|| format!("inspect opened {}", source.display()))?
            .is_file()
        {
            anyhow::bail!("refusing non-regular copy source: {}", source.display());
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .with_context(|| format!("read {}", source.display()))?;
        atomic_write(destination_root, destination, &contents)
    }

    pub fn validate_destination(trusted_root: &Path, path: &Path) -> Result<()> {
        let (parent, file_name) = open_parent(trusted_root, path, true)?;
        validate_entry(parent.as_raw_fd(), &file_name, path)
    }

    fn open_parent(trusted_root: &Path, path: &Path, create: bool) -> Result<(File, CString)> {
        let relative = normalized_relative(trusted_root, path)?;
        let file_name = relative
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("file has no name: {}", path.display()))?;
        let mut directory = open_root(trusted_root)?;
        if let Some(parent) = relative.parent() {
            for component in parent.components() {
                let name = c_string(component.as_os_str())?;
                match openat_directory(directory.as_raw_fd(), &name) {
                    Ok(next) => directory = next,
                    Err(error)
                        if create
                            && error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                                error.kind() == std::io::ErrorKind::NotFound
                            }) =>
                    {
                        let created =
                            unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o755) };
                        if created != 0 {
                            let error = std::io::Error::last_os_error();
                            if error.kind() != std::io::ErrorKind::AlreadyExists {
                                return Err(error).with_context(|| {
                                    format!("create directory below {}", trusted_root.display())
                                });
                            }
                        }
                        directory =
                            openat_directory(directory.as_raw_fd(), &name).with_context(|| {
                                format!("open created directory below {}", trusted_root.display())
                            })?;
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "refusing symlink ancestor or invalid ancestor below trusted root {}",
                                trusted_root.display()
                            )
                        });
                    }
                }
            }
        }
        Ok((directory, c_string(file_name)?))
    }

    fn normalized_relative<'a>(trusted_root: &Path, path: &'a Path) -> Result<&'a Path> {
        let relative = path.strip_prefix(trusted_root).with_context(|| {
            format!(
                "destination {} is outside trusted root {}",
                path.display(),
                trusted_root.display()
            )
        })?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            anyhow::bail!("path is not a normalized descendant: {}", path.display());
        }
        Ok(relative)
    }

    fn open_root(path: &Path) -> Result<File> {
        let encoded = c_string(path.as_os_str())?;
        let fd = unsafe {
            libc::open(
                encoded.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        owned_file(fd).with_context(|| format!("open trusted root {}", path.display()))
    }

    fn openat_directory(parent: RawFd, name: &CString) -> Result<File> {
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        owned_file(fd)
    }

    fn openat_file(
        parent: RawFd,
        name: &CString,
        flags: libc::c_int,
        mode: libc::mode_t,
    ) -> Result<File> {
        let fd = unsafe { libc::openat(parent, name.as_ptr(), flags, libc::c_uint::from(mode)) };
        owned_file(fd)
    }

    fn owned_file(fd: RawFd) -> Result<File> {
        if fd < 0 {
            Err(std::io::Error::last_os_error().into())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    fn validate_entry(parent: RawFd, name: &CString, path: &Path) -> Result<()> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                parent,
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(error).with_context(|| format!("inspect {}", path.display()));
        }
        let mode = unsafe { stat.assume_init().st_mode } & libc::S_IFMT;
        if mode == libc::S_IFLNK {
            anyhow::bail!("refusing symlink destination: {}", path.display());
        }
        if mode != libc::S_IFREG {
            anyhow::bail!("destination is not a regular file: {}", path.display());
        }
        Ok(())
    }

    fn c_string(value: &OsStr) -> Result<CString> {
        CString::new(value.as_bytes()).context("path contains a NUL byte")
    }
}

pub fn validate_write_target(trusted_root: &Path, path: &Path) -> Result<()> {
    #[cfg(unix)]
    return unix_private_io::validate_destination(trusted_root, path);

    #[cfg(not(unix))]
    {
        validate_write_target_fallback(trusted_root, path)
    }
}

#[cfg(not(unix))]
fn validate_write_target_fallback(trusted_root: &Path, path: &Path) -> Result<()> {
    validate_path_below_root(trusted_root, path, true)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("refusing symlink destination: {}", path.display())
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!("destination is not a regular file: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

#[cfg(not(unix))]
fn validate_path_below_root(trusted_root: &Path, path: &Path, create_parents: bool) -> Result<()> {
    let relative = path.strip_prefix(trusted_root).with_context(|| {
        format!(
            "destination {} is outside trusted root {}",
            path.display(),
            trusted_root.display()
        )
    })?;
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        anyhow::bail!("path is not a normalized descendant: {}", path.display());
    }
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    let mut current = trusted_root.to_path_buf();
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("refusing symlink ancestor: {}", current.display())
            }
            Ok(metadata) if !metadata.is_dir() => {
                anyhow::bail!("path ancestor is not a directory: {}", current.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_parents => {
                fs::create_dir(&current)
                    .with_context(|| format!("create {}", current.display()))?;
                let metadata = fs::symlink_metadata(&current)
                    .with_context(|| format!("inspect {}", current.display()))?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    anyhow::bail!(
                        "created path ancestor is not a directory: {}",
                        current.display()
                    );
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

pub fn confirm(prompt: &str) -> Result<bool> {
    use std::io::{self, Write};

    eprint!("{prompt}");
    io::stderr().flush().ok();

    let mut s = String::new();
    io::stdin().read_line(&mut s).context("read user input")?;
    let ans = s.trim().to_lowercase();
    Ok(ans == "y" || ans == "yes")
}

pub fn resolve_worktree_name(
    state: &State,
    name: Option<&str>,
    override_name: Option<&str>,
) -> Option<String> {
    override_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .or_else(|| name.map(str::trim).filter(|name| !name.is_empty()))
        .map(str::to_string)
        .or_else(|| infer_worktree_from_cwd(state))
}

fn infer_worktree_from_cwd(st: &State) -> Option<String> {
    let wd = env::current_dir().ok()?;
    let wd = wd.canonicalize().unwrap_or(wd);
    for a in st.allocations.values() {
        let ap = PathBuf::from(&a.path);
        let ap = ap.canonicalize().unwrap_or(ap);
        if wd.strip_prefix(&ap).is_ok() {
            return Some(a.name.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_regular_and_dangling_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::write(&target, "original").unwrap();
        let link = directory.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(atomic_write_private(directory.path(), &link, b"changed").is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");

        let dangling = directory.path().join("dangling");
        symlink(directory.path().join("missing"), &dangling).unwrap();
        assert!(atomic_write_private(directory.path(), &dangling, b"changed").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_copy_rejects_symlink_sources_and_writes_mode_600() {
        use std::os::unix::fs::{MetadataExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::write(&source, "secret").unwrap();
        let link = directory.path().join("source-link");
        symlink(&source, &link).unwrap();
        assert!(
            atomic_copy_private(
                directory.path(),
                &link,
                directory.path(),
                &directory.path().join("nope")
            )
            .is_err()
        );

        let destination = directory.path().join("destination");
        atomic_copy_private(directory.path(), &source, directory.path(), &destination).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "secret");
        assert_eq!(fs::metadata(destination).unwrap().mode() & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_copy_rejects_fifo_sources_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source-fifo");
        let encoded = CString::new(source.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);

        let destination = directory.path().join("destination");
        let error = atomic_copy_private(directory.path(), &source, directory.path(), &destination)
            .unwrap_err();

        assert!(error.to_string().contains("non-regular copy source"));
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_symlink_ancestors_below_only_the_trusted_root() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        let actual_root = outside.path().join("actual-root");
        fs::create_dir(&actual_root).unwrap();
        let root_link = outside.path().join("trusted-root-link");
        symlink(&actual_root, &root_link).unwrap();
        let escaped = outside.path().join("escaped");
        fs::create_dir(&escaped).unwrap();
        symlink(&escaped, actual_root.join("nested")).unwrap();

        let destination = root_link.join("nested/secret");
        let error = atomic_write_private(&root_link, &destination, b"secret").unwrap_err();

        assert!(error.to_string().contains("symlink ancestor"), "{error:#}");
        assert!(!escaped.join("secret").exists());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_allows_a_symlink_at_the_explicit_trusted_root() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let actual_root = directory.path().join("actual-root");
        fs::create_dir(&actual_root).unwrap();
        let trusted_root = directory.path().join("trusted-root");
        symlink(&actual_root, &trusted_root).unwrap();

        atomic_write_private(
            &trusted_root,
            &trusted_root.join("nested/private.env"),
            b"secret",
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(actual_root.join("nested/private.env")).unwrap(),
            "secret"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_copy_rejects_symlink_ancestors_in_both_roots() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let source_root = directory.path().join("source-root");
        let destination_root = directory.path().join("destination-root");
        let outside = directory.path().join("outside");
        fs::create_dir(&source_root).unwrap();
        fs::create_dir(&destination_root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), "secret").unwrap();

        symlink(&outside, source_root.join("nested")).unwrap();
        assert!(
            atomic_copy_private(
                &source_root,
                &source_root.join("nested/secret"),
                &destination_root,
                &destination_root.join("copied")
            )
            .is_err()
        );

        fs::write(source_root.join("source"), "secret").unwrap();
        symlink(&outside, destination_root.join("nested")).unwrap();
        assert!(
            atomic_copy_private(
                &source_root,
                &source_root.join("source"),
                &destination_root,
                &destination_root.join("nested/copied")
            )
            .is_err()
        );
        assert!(!outside.join("copied").exists());
    }
}
