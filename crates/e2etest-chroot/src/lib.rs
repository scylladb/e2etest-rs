/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

//! This crate provides functionality for e2etest tests to manage running services in chroot
//! environment based on container images.

use nix::errno::Errno;
use nix::fcntl;
use nix::fcntl::OFlag;
use nix::mount;
use nix::mount::MsFlags;
use nix::sched;
use nix::sys::stat::Mode;
use nix::unistd;
use std::ffi::CString;
use std::fs::Permissions;
use std::io;
use std::iter;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use tap::Tap;
use tempfile::TempDir;
use testcontainers::ContainerAsync;
use testcontainers::GenericImage;
use testcontainers::ImageExt;
use testcontainers::bollard::plugin::Mount;
use testcontainers::bollard::plugin::MountBindOptions;
use testcontainers::bollard::plugin::MountBindOptionsPropagationEnum;
use testcontainers::bollard::plugin::MountType;
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use tokio::fs;
use tokio::process::Command;
use tokio::runtime::Builder;
use tracing::error;

enum ChrootError {
    Unshare = 1,
    SetHostname,
    MountRoot,
    MountOverlay,
    MkDir,
    Touch,
    MountBind,
    Chroot,
    Chdir,
}

impl ChrootError {
    const MASK_LEN: u32 = 24;
    const MASK_SYS_ERROR: i32 = (u32::MAX >> (u32::BITS - Self::MASK_LEN)) as i32;
    const MASK_CHROOT_ERROR: i32 = !Self::MASK_SYS_ERROR;

    fn encode(self, err: io::Error) -> io::Error {
        let errno = err.raw_os_error().unwrap_or(0) & Self::MASK_SYS_ERROR;
        io::Error::from_raw_os_error(errno | ((self as i32) << Self::MASK_LEN))
    }

    fn decode(err: io::Error) -> (Option<Self>, io::Error) {
        let Some(errno) = err.raw_os_error() else {
            return (None, err);
        };
        let err_chroot = errno & Self::MASK_CHROOT_ERROR;
        if err_chroot == 0 {
            return (None, err);
        }
        (
            match err_chroot >> Self::MASK_LEN {
                x if x == Self::Unshare as i32 => Some(Self::Unshare),
                x if x == Self::SetHostname as i32 => Some(Self::SetHostname),
                x if x == Self::MountRoot as i32 => Some(Self::MountRoot),
                x if x == Self::MountOverlay as i32 => Some(Self::MountOverlay),
                x if x == Self::MountBind as i32 => Some(Self::MountBind),
                x if x == Self::MkDir as i32 => Some(Self::MkDir),
                x if x == Self::Touch as i32 => Some(Self::Touch),
                x if x == Self::Chroot as i32 => Some(Self::Chroot),
                x if x == Self::Chdir as i32 => Some(Self::Chdir),
                _ => None,
            },
            io::Error::from_raw_os_error(errno & Self::MASK_SYS_ERROR),
        )
    }
}

/// Returns a human-readable description of the error, including the underlying system error.
pub fn describe_command_error(err: io::Error) -> String {
    let (unshare_err, err) = ChrootError::decode(err);
    match unshare_err {
        Some(ChrootError::Unshare) => {
            format!("Failed to unshare: {err}")
        }
        Some(ChrootError::SetHostname) => {
            format!("Failed to set hostname: {err}")
        }
        Some(ChrootError::MountRoot) => {
            format!("Failed to mount private root: {err}")
        }
        Some(ChrootError::MountOverlay) => {
            format!("Failed to mount overlay root: {err}")
        }
        Some(ChrootError::MkDir) => {
            format!("Failed to make directory: {err}")
        }
        Some(ChrootError::Touch) => {
            format!("Failed to touch file: {err}")
        }
        Some(ChrootError::MountBind) => {
            format!("Failed to mount bind: {err}")
        }
        Some(ChrootError::Chroot) => {
            format!("Failed to chroot: {err}")
        }
        Some(ChrootError::Chdir) => {
            format!("Failed to chdir: {err}")
        }
        None => {
            format!("{err}")
        }
    }
}

/// Represents a bind mount from a source path to a destination path.
pub struct BindMount<'a> {
    pub src: &'a Path,
    pub dst: &'a Path,
}

impl<'a> BindMount<'a> {
    /// Creates a new `BindMount` with the given source and destination paths.
    pub fn new(src: &'a Path, dst: &'a Path) -> Self {
        Self { src, dst }
    }
}

/// Trait to extend `Command` with the ability to run within a chroot environment.
pub trait WithinChroot {
    /// Configures the command to run within a chroot environment using the specified overlay
    /// filesystem, root filesystem, and bind mounts.
    ///
    /// `rootfs` is the overlay read-only layer, so it must stay unchanged while the process is
    /// running.
    fn within_chroot<'a>(
        &mut self,
        overlay: Overlay,
        rootfs: &Path,
        binds: impl IntoIterator<Item = BindMount<'a>>,
    ) -> &mut Self;
}

impl WithinChroot for Command {
    fn within_chroot<'a>(
        &mut self,
        overlay: Overlay,
        rootfs: &Path,
        binds: impl IntoIterator<Item = BindMount<'a>>,
    ) -> &mut Self {
        assert_valid_path_for_overlay(rootfs);
        let mut dirs_to_create = vec![];
        let mut files_to_create = vec![];
        let mut add_recursive = |path: &Path, file: bool| {
            let parent = if let Some(parent) = path.parent() {
                parent.iter().fold(rootfs.to_path_buf(), |acc, p| {
                    let dir = acc.join(p);
                    dirs_to_create.push(
                        CString::new(dir.to_string_lossy().as_bytes())
                            .expect("within_chroot: failed to create CString for directory"),
                    );
                    dir
                })
            } else {
                rootfs.to_path_buf()
            };
            if file {
                files_to_create.push(
                    CString::new(
                        parent
                            .join(path.file_name().unwrap())
                            .to_string_lossy()
                            .as_bytes(),
                    )
                    .expect("within_chroot: failed to create CString for file"),
                );
            } else {
                dirs_to_create.push(
                    CString::new(
                        parent
                            .join(path.file_name().unwrap())
                            .to_string_lossy()
                            .as_bytes(),
                    )
                    .expect("within_chroot: failed to create CString for directory"),
                );
            }
        };

        add_recursive(Path::new("proc"), false);
        add_recursive(Path::new("sys"), false);
        add_recursive(Path::new("dev"), false);
        add_recursive(Path::new("etc/resolv.conf"), true);

        let binds: Vec<_> = iter::once((
            CString::new("/proc").expect("within_chroot: failed to create CString for /proc"),
            CString::new(rootfs.join("proc").to_string_lossy().as_bytes())
                .expect("within_chroot: failed to create CString for /proc mount point"),
        ))
        .chain(iter::once((
            CString::new("/sys").expect("within_chroot: failed to create CString for /sys"),
            CString::new(rootfs.join("sys").to_string_lossy().as_bytes())
                .expect("within_chroot: failed to create CString for /sys mount point"),
        )))
        .chain(iter::once((
            CString::new("/dev").expect("within_chroot: failed to create CString for /dev"),
            CString::new(rootfs.join("dev").to_string_lossy().as_bytes())
                .expect("within_chroot: failed to create CString for /dev mount point"),
        )))
        .chain(iter::once((
            CString::new("/etc/resolv.conf")
                .expect("within_chroot: failed to create CString for /etc/resolv.conf"),
            CString::new(
                rootfs
                    .join("etc")
                    .join("resolv.conf")
                    .to_string_lossy()
                    .as_bytes(),
            )
            .expect("within_chroot: failed to create CString for /etc/resolv.conf mount point"),
        )))
        .chain(binds.into_iter().map(|bind| {
            let dst = bind.dst.strip_prefix("/").unwrap_or(bind.dst);
            add_recursive(dst, bind.src.is_file());
            (
                CString::new(bind.src.to_string_lossy().as_bytes())
                    .expect("within_chroot: failed to create CString for bind source"),
                CString::new(rootfs.join(dst).to_string_lossy().as_bytes())
                    .expect("within_chroot: failed to create CString for bind destination"),
            )
        }))
        .collect();

        // Sort and deduplicate the directories and files to create
        dirs_to_create.sort();
        dirs_to_create.dedup();
        files_to_create.sort();
        files_to_create.dedup();

        let overlay_options = CString::new(
            format!(
                "lowerdir={rootfs},upperdir={upperdir},workdir={workdir},userxattr",
                rootfs = rootfs.to_string_lossy(),
                upperdir = overlay.upperdir.to_string_lossy(),
                workdir = overlay.workdir.to_string_lossy()
            )
            .as_bytes(),
        )
        .expect("within_chroot: failed to create CString for overlay options");
        let rootfs = CString::new(rootfs.to_string_lossy().as_bytes())
            .expect("within_chroot: failed to create CString for rootfs");

        unsafe {
            // SAFETY: We must not allocate memory in this closure, as it is called after fork()
            // and before exec().
            self.pre_exec(move || {
                sched::unshare(sched::CloneFlags::CLONE_NEWNS | sched::CloneFlags::CLONE_NEWUTS)
                    .map_err(|err| ChrootError::Unshare.encode(err.into()))?;

                unistd::sethostname("e2etest-chroot")
                    .map_err(|e| ChrootError::SetHostname.encode(e.into()))?;

                // Stop mount events from propagating back to the host
                mount::mount(
                    None::<&str>,
                    &b"/"[..],
                    None::<&str>,
                    MsFlags::MS_REC | MsFlags::MS_PRIVATE,
                    None::<&str>,
                )
                .map_err(|err| ChrootError::MountRoot.encode(err.into()))?;

                mount::mount(
                    Some(&b"overlay"[..]),
                    rootfs.as_c_str(),
                    Some(&b"overlay"[..]),
                    MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
                    Some(overlay_options.as_c_str()),
                )
                .map_err(|err| ChrootError::MountOverlay.encode(err.into()))?;

                for dir in dirs_to_create.iter() {
                    if let Err(err) = unistd::mkdir(dir.as_c_str(), Mode::from_bits_truncate(0o755))
                        && err != Errno::EEXIST
                    {
                        return Err(ChrootError::MkDir.encode(err.into()));
                    }
                }
                for file in files_to_create.iter() {
                    fcntl::open(
                        file.as_c_str(),
                        OFlag::O_CREAT | OFlag::O_RDONLY,
                        Mode::from_bits_truncate(0o644),
                    )
                    .map_err(|err| ChrootError::Touch.encode(err.into()))?;
                }

                for (src, dst) in binds.iter() {
                    mount::mount(
                        Some(src.as_c_str()),
                        dst.as_c_str(),
                        None::<&str>,
                        MsFlags::MS_BIND | MsFlags::MS_REC,
                        None::<&str>,
                    )
                    .map_err(|err| ChrootError::MountBind.encode(err.into()))?;
                }

                unistd::chroot(rootfs.as_c_str())
                    .map_err(|err| ChrootError::Chroot.encode(err.into()))?;

                unistd::chdir(&b"/"[..]).map_err(|err| ChrootError::Chdir.encode(err.into()))?;

                Ok(())
            });
        }
        self
    }
}

/// Represents an overlay filesystem with upper and work directories.
pub struct Overlay {
    upperdir: PathBuf,
    workdir: PathBuf,
}

/// Represents a guard that ensures the temporary directories for the overlay filesystem are
/// cleaned up.
///
/// The `OverlayGuard` must be kept alive until the process started with the corresponding
/// `Overlay` has exited.
pub struct OverlayGuard {
    _upperdir: TempDir,
    _workdir: TempDir,
}

impl Overlay {
    /// Creates a new `Overlay` with temporary upper and work directories within the specified
    /// `tempdir`.
    pub async fn new(tempdir: &Path) -> (Self, OverlayGuard) {
        assert_valid_path_for_overlay(tempdir);

        let upperdir = TempDir::new_in(tempdir)
            .expect("Overlay::new: failed to create temporary directory for upperdir");
        fs::set_permissions(upperdir.path(), Permissions::from_mode(0o755))
            .await
            .expect("Overlay::new: failed to set permissions for upperdir");
        let workdir = TempDir::new_in(tempdir)
            .expect("Overlay::new: failed to create temporary directory for workdir");
        (
            Overlay {
                upperdir: upperdir.path().to_path_buf(),
                workdir: workdir.path().to_path_buf(),
            },
            OverlayGuard {
                _upperdir: upperdir,
                _workdir: workdir,
            },
        )
    }
}

struct DockerExport {
    dir: TempDir,
    container: ContainerAsync<GenericImage>,
}

/// Represents a Docker root filesystem exported from a running container.
pub struct DockerRootfs {
    name_tag: String,
    docker_export: Option<DockerExport>,
}

impl DockerRootfs {
    const CONTAINER_DIR: &str = "/e2etest-chroot";
    const ROOTFS_DIR: &str = "rootfs";

    /// Creates a new `DockerRootfs` by starting a container from the specified image name and tag,
    /// and exporting its root filesystem to a temporary directory within the specified `tempdir`.
    ///
    /// The specified `tempdir` must exist for the lifetime of the `DockerRootfs` instance, as it
    /// is used for the mount for the container.
    pub async fn new(name: &str, tag: &str, tempdir: &Path) -> DockerRootfs {
        assert_valid_path_for_overlay(tempdir);

        let name_tag = format!("{name}:{tag}");

        let dir = TempDir::new_in(tempdir).unwrap_or_else(|err| {
            panic!(
                "DockerRootfs::new: failed to create temporary directory \
                for {name_tag} docker export: {err}"
            )
        });
        let rootfs = dir.path().join(Self::ROOTFS_DIR);
        fs::create_dir_all(&rootfs)
            .await
            .expect("DockerRootfs::new: failed to create rootfs directory");

        let image = GenericImage::new(name, tag);

        let mount = Mount::default().tap_mut(|m| {
            m.source = Some(dir.path().to_string_lossy().to_string());
            m.target = Some(Self::CONTAINER_DIR.to_string());
            m.typ = Some(MountType::BIND);
            m.bind_options = Some(MountBindOptions::default().tap_mut(|bo| {
                bo.propagation = Some(MountBindOptionsPropagationEnum::RSHARED);
            }));
        });
        let container = image
            .with_entrypoint("sh")
            .with_wait_for(WaitFor::message_on_stdout("e2etest-chroot: rootfs mounted"))
            .with_privileged(true)
            .with_user("root")
            .with_cmd([
                "-c".to_string(),
                format!(
                    "trap 'umount {container_dir}/{rootfs_dir} ; exit 0' TERM ; \
                    mount --bind / {container_dir}/{rootfs_dir} \
                    && echo \"e2etest-chroot: rootfs mounted\" \
                    && {{ sleep infinity & wait ; }}",
                    container_dir = Self::CONTAINER_DIR,
                    rootfs_dir = Self::ROOTFS_DIR
                ),
            ])
            .with_host_config_modifier(move |hc| {
                hc.mounts.get_or_insert_with(Vec::new).push(mount.clone());
            })
            .start()
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "DockerRootfs::new: failed to start {name_tag} container \
                    to export rootfs: {err}"
                )
            });
        assert!(
            fs::try_exists(rootfs.join("bin"))
                .await
                .expect("DockerRootfs::new: failed to check if rootfs directory is exported"),
            "DockerRootfs::new: rootfs directory does not exist after starting \
            {name_tag} container. The tempdir must be on a shared mount, \
            and the namespace must receive mount events from the host (slave not private)."
        );
        Self {
            name_tag,
            docker_export: Some(DockerExport { dir, container }),
        }
    }

    /// Stops the running container and cleans up the temporary directory used for exporting the
    /// root filesystem. If stopping the container fails, the temporary directory will not be
    /// deleted and you will need to delete it manually.
    pub async fn stop(mut self) {
        if let Some(DockerExport { dir, container }) = self.docker_export.take()
            && let Err(err) = container.stop().await
        {
            error!(
                "Failed to stop {name_tag} container: {err}. \
                The temporary directory won't be deleted, you can delete it manually: {dir:?}",
                name_tag = self.name_tag,
            );
            _ = dir.keep();
        }
    }

    /// Returns the path to the exported root filesystem directory.
    pub fn rootfs_path(&self) -> PathBuf {
        self.docker_export
            .as_ref()
            .expect("DockerRootfs::rootfs_path: docker_export is None")
            .dir
            .path()
            .join(Self::ROOTFS_DIR)
    }
}

impl Drop for DockerRootfs {
    fn drop(&mut self) {
        if let Some(docker_export) = self.docker_export.take() {
            let name_tag = self.name_tag.clone();
            _ = thread::spawn(move || {
                let Ok(runtime) = Builder::new_current_thread().enable_all().build() else {
                    error!(
                        "Failed to create tokio runtime to stop {name_tag} container. \
                        The {name_tag} container won't be stopped \
                        and the temporary directory won't be deleted, \
                        you can delete it manually: {dir:?}",
                        dir = docker_export.dir,
                    );
                    _ = docker_export.dir.keep();
                    return;
                };
                runtime.block_on(async move {
                    DockerRootfs {
                        name_tag,
                        docker_export: Some(docker_export),
                    }
                    .stop()
                    .await;
                });
            })
            .join();
        }
    }
}

/// Extracts the Docker image name and tag from a given string. If the tag is not specified, it
/// defaults to "latest".
pub fn extract_docker_name_tag(name: &str) -> (&str, &str) {
    if let Some((name, tag)) = name.rsplit_once(':')
        && !tag.contains('/')
    {
        (name, tag)
    } else {
        (name, "latest")
    }
}

fn assert_valid_path_for_overlay(path: &Path) {
    assert!(
        !path.to_string_lossy().contains(','),
        "path {path:?} for overlay mount for container contains a comma, which is not allowed"
    );
    assert!(
        !path.to_string_lossy().contains(':'),
        "path {path:?} for overlay mount for container contains a colon, which is not allowed"
    );
}
