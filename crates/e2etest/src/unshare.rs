/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use crate::Restricted;
use crate::Statistics;
use ipc_channel::ipc::IpcOneShotServer;
use neli::consts::nl::NlmF;
use neli::consts::rtnl::Rtm;
use neli::consts::socket::NlFamily;
use neli::nl::NlPayload;
use neli::router::synchronous::NlRouter;
use neli::rtnl::Ifinfomsg;
use neli::rtnl::IfinfomsgBuilder;
use neli::utils::Groups;
use nix::mount;
use nix::mount::MsFlags;
use nix::sched;
use nix::unistd;
use std::env;
use std::fs;
use std::io;
use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::process::Child;
use std::process::Command;
use tracing::error;
use tracing::info;

const UNSHARE_INFO_ENV: &str = "E2ETEST_UNSHARE_INFO";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UnshareInfo {
    pub(crate) restricted: Restricted,
    pub(crate) filters: Vec<String>,
    pub(crate) ipc_channel: String,
}

impl UnshareInfo {
    /// Returns the name of the test if the process is restricted to a specific test.
    pub fn test_name(&self) -> Option<&str> {
        match &self.restricted {
            Restricted::None | Restricted::Group(_) => None,
            Restricted::Test(name) => Some(name),
        }
    }

    /// Returns the name of the group if the process is restricted to a specific group.
    pub fn group_name(&self) -> Option<&str> {
        match &self.restricted {
            Restricted::None | Restricted::Test(_) => None,
            Restricted::Group(name) => Some(name),
        }
    }
}

pub fn unshare_info() -> Option<UnshareInfo> {
    env::var(UNSHARE_INFO_ENV)
        .ok()
        .map(|s| serde_json::from_str(&s).expect("Failed to deserialize unshare info"))
}

enum UnshareError {
    Unshare = 1,
    SetGroups,
    UidMap,
    GidMap,
    SetHostname,
    MountRoot,
}

impl UnshareError {
    const MASK_LEN: u32 = 24;
    const MASK_SYS_ERROR: i32 = (u32::MAX >> (u32::BITS - Self::MASK_LEN)) as i32;
    const MASK_UNSHARE_ERROR: i32 = !Self::MASK_SYS_ERROR;

    fn encode(self, err: io::Error) -> io::Error {
        let errno = err.raw_os_error().unwrap_or(0) & Self::MASK_SYS_ERROR;
        io::Error::from_raw_os_error(errno | ((self as i32) << Self::MASK_LEN))
    }

    fn decode(err: io::Error) -> (Option<Self>, io::Error) {
        let Some(errno) = err.raw_os_error() else {
            return (None, err);
        };
        let err_unshare = errno & Self::MASK_UNSHARE_ERROR;
        if err_unshare == 0 {
            return (None, err);
        }
        (
            match err_unshare >> Self::MASK_LEN {
                x if x == Self::Unshare as i32 => Some(Self::Unshare),
                x if x == Self::SetGroups as i32 => Some(Self::SetGroups),
                x if x == Self::UidMap as i32 => Some(Self::UidMap),
                x if x == Self::GidMap as i32 => Some(Self::GidMap),
                x if x == Self::SetHostname as i32 => Some(Self::SetHostname),
                x if x == Self::MountRoot as i32 => Some(Self::MountRoot),
                _ => None,
            },
            io::Error::from_raw_os_error(errno & Self::MASK_SYS_ERROR),
        )
    }
}

pub fn register_apparmor_profile() -> String {
    let path = env::current_exe().expect("Failed to get current executable");
    format!(
        r"
cat <<'EOF' | sudo apparmor_parser -r
abi <abi/4.0>,
include <tunables/global>

profile {name:?} {path:?} flags=(unconfined) {{
  userns,
}}
EOF
",
        name = path
            .file_name()
            .expect("Failed to get current executable file name")
    )
}

fn panic_on_spawn_error(name: &str, err: io::Error) -> ! {
    let (unshare_err, err) = UnshareError::decode(err);
    match unshare_err {
        Some(UnshareError::Unshare) => {
            error!("{name}: Failed to unshare: {err}");
        }
        Some(UnshareError::SetGroups) => {
            error!("{name}: Failed to write to /proc/self/setgroups: {err}");
            if err.kind() == ErrorKind::PermissionDenied {
                error!(
                    "\
                        This may be due to certain security restrictions. \
                        On Ubuntu check if `sysctl kernel.apparmor_restrict_unprivileged_userns` \
                        is set. If so, you may need to disable it (in temporary CI machine) \
                        using `sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0` \
                        or prepare a correct AppArmor profile for the binary or run with sudo. \
                        On Ubuntu you can register the profile with this:\n{}
                        ",
                    register_apparmor_profile()
                );
            }
        }
        Some(UnshareError::UidMap) => {
            error!("{name}: Failed to write to /proc/self/uid_map: {err}");
        }
        Some(UnshareError::GidMap) => {
            error!("{name}: Failed to write to /proc/self/gid_map: {err}");
        }
        Some(UnshareError::SetHostname) => {
            error!("{name}: Failed to set hostname: {err}");
        }
        Some(UnshareError::MountRoot) => {
            error!("{name}: Failed to mount private root: {err}");
        }
        None => {}
    }
    panic!("{name}: Failed to spawn and unshare process: {err}");
}

pub(crate) fn spawn(
    restricted: Restricted,
    filters: Vec<String>,
) -> (Child, IpcOneShotServer<Statistics>) {
    let (rx, ipc_channel) =
        IpcOneShotServer::<Statistics>::new().expect("Failed to create IPC channel");

    let uid = unistd::getuid();
    let gid = unistd::getgid();
    let uid_map = format!("0 {uid} 1").as_bytes().to_vec();
    let gid_map = format!("0 {gid} 1").as_bytes().to_vec();

    let mut cmd = Command::new(env::current_exe().expect("Failed to get current executable"));
    cmd.args(env::args_os().skip(1)).env(
        UNSHARE_INFO_ENV,
        serde_json::to_string(&UnshareInfo {
            restricted: restricted.clone(),
            filters,
            ipc_channel,
        })
        .expect("Failed to serialize unshare info"),
    );

    unsafe {
        // SAFETY: We must not allocate memory in this closure, as it is called after fork()
        // and before exec().
        cmd.pre_exec(move || {
            sched::unshare(
                sched::CloneFlags::CLONE_NEWUSER
                    | sched::CloneFlags::CLONE_NEWNET
                    | sched::CloneFlags::CLONE_NEWNS
                    | sched::CloneFlags::CLONE_NEWUTS,
            )
            .map_err(|err| UnshareError::Unshare.encode(err.into()))?;

            if !uid.is_root() {
                fs::write("/proc/self/setgroups", b"deny")
                    .map_err(|err| UnshareError::SetGroups.encode(err))?;
                fs::write("/proc/self/uid_map", &uid_map)
                    .map_err(|err| UnshareError::UidMap.encode(err))?;
                fs::write("/proc/self/gid_map", &gid_map)
                    .map_err(|err| UnshareError::GidMap.encode(err))?;
            }

            unistd::sethostname("e2etest-sandbox")
                .map_err(|e| UnshareError::SetHostname.encode(e.into()))?;

            // Stop mount events from propagating back to the host
            mount::mount(
                None::<&str>,
                "/",
                None::<&str>,
                MsFlags::MS_REC | MsFlags::MS_PRIVATE,
                None::<&str>,
            )
            .map_err(|err| UnshareError::MountRoot.encode(err.into()))?;

            Ok(())
        });
    }

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            let name = match restricted {
                Restricted::None => "(none)".to_string(),
                Restricted::Group(name) => name,
                Restricted::Test(name) => name,
            };
            panic_on_spawn_error(&name, err);
        }
    };

    (child, rx)
}

fn init_lo() {
    info!("Initializing loopback interface in unshared network namespace");
    // The index of the loopback interface ought to be always 1 in the unshared network namespace
    const LO_IFINDEX: i32 = 1;

    let (rtnl, _) = NlRouter::connect(NlFamily::Route, None, Groups::empty())
        .inspect_err(|err| {
            error!("Failed to connect to rtnetlink: {err}");
        })
        .expect("Failed to connect to rtnetlink");

    let msg = IfinfomsgBuilder::default()
        .ifi_family(neli::consts::rtnl::RtAddrFamily::Unspecified)
        .ifi_index(LO_IFINDEX)
        .up()
        .build()
        .inspect_err(|err| {
            error!("Failed to build Ifinfomsg: {err}");
        })
        .expect("Failed to build Ifinfomsg");

    let responses = rtnl
        .send::<_, _, Rtm, Ifinfomsg>(
            Rtm::Newlink,
            NlmF::REQUEST | NlmF::ACK,
            NlPayload::Payload(msg),
        )
        .inspect_err(|err| {
            error!("Failed to send Newlink message: {err}");
        })
        .expect("Failed to send Newlink message");

    for response in responses {
        response
            .inspect_err(|err| {
                error!("Response from rtnetlink was an error: {err}");
            })
            .expect("Response from rtnetlink was an error");
    }
}

pub(crate) fn init_namespaces() {
    if !unistd::getuid().is_root() {
        // If we are not root we are not using namespaces
        return;
    }
    // The loopback interface is not automatically created in a new network namespace, so we need
    // to create it manually.
    init_lo();
}
