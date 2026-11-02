/*
 * Copyright 2025-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

//! This library provides a framework for defining and running End-to-End tests on network service
//! for Rust. It allows users to define test cases with multiple tests, and provides a global
//! fixture for all of them.
//!
//! ## Basic Concepts
//!
//! The library divides running tests into two phases: the first phase called by `run()` divides
//! the tests into groups and standalone tests, and spawns a separate process for each group or
//! standalone test in a separate process with newly created namespace. The second phase called by
//! `run_in_unshare()` runs the tests restricted by the first phase in the unshare environment, and
//! transmits the statistics back to the parent process. The binary which uses this library should
//! call `run()` or `run_in_unshare()` in the main function as fast as possible (it can call
//! them after parsing command line arguments to prepare the configuration) - both functions are
//! early entry points for the process. By using `unshare_info()` the binary can detect the phase
//! of execution. The binary should be aware that it will be spawned, by the first phase, with the
//! same arguments.
//!
//! If the binary doesn't want to use unshare, it can call `run_without_unshare()` instead of
//! `run()` - this will run all tests and groups in the same process without unshare namespaces.
//!
//! The crate creates its own tokio runtime in `run_in_unshare()` or `run_without_unshare()`.
//!
//! ## Unshare security setup
//!
//! The library creates these linux namespaces for child processes: user, network, mount, UTS.
//! The crate remaps the user id and group id of the current user to the root user in the new user
//! namespace. The crate creates a new network namespace and sets up a loopback interface. The
//! crate sets up a new hostname in the new UTS namespace.
//!
//! Some distributions (e.g. Ubuntu) have AppArmor enabled by default, which prevents unshare from
//! working. To allow unshare to work, either register an AppArmor profile or disable AppArmor for
//! restrictions on user namespace creation.
//!
//! Disabling AppArmor restrictions can be done by running the following command:
//! ```bash
//! sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0
//! ```
//!
//! Registering AppArmor profile can be done by running the following command:
//! ```bash
//! cat <<'EOF' | sudo apparmor_parser -r
//! abi <abi/4.0>,
//! include <tunables/global>
//!
//! profile validator-bin /path/to/validator-bin flags=(unconfined) {
//!   userns,
//! }
//! EOF
//! ```
//!
//! The crate provides a helper function `apparmor_profile_command()` which can provide the
//! AppArmor profile for the current binary.
//!
//! ## Usage
//!
//! See this simple example:
//!
//! ```rust
//! mod sample {
//!
//! use std::net::Ipv4Addr;
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! #[derive(Clone, Copy)]
//! pub struct FixtureCfg {
//!     pub dns_ip: Ipv4Addr,
//! }
//!
//! #[derive(Clone, Copy)]
//! pub struct FixtureOne {
//!     dns_ip: Ipv4Addr,
//! }
//!
//! impl e2etest::Fixture for FixtureOne {
//!     async fn setup(setup: &mut impl e2etest::Setup) -> Option<Self> {
//!         let cfg = setup.get::<FixtureCfg>().await.unwrap();
//!         Some(Self { dns_ip: cfg.dns_ip })
//!     }
//!
//!     async fn teardown(self) { }
//! }
//!
//! #[derive(Clone, Copy)]
//! pub struct FixtureTwo {
//!     octet: u8,
//! }
//!
//! impl e2etest::Fixture for FixtureTwo {
//!     async fn setup(setup: &mut impl e2etest::Setup) -> Option<Self> {
//!         let one = setup.setup::<FixtureOne>().await?;
//!         Some(Self { octet: one.dns_ip.octets()[2] })
//!     }
//!
//!     async fn teardown(self) { }
//! }
//!
//! #[derive(Clone, Copy)]
//! pub struct FixtureThree {
//!     number: usize,
//! }
//!
//! impl e2etest::Fixture for FixtureThree {
//!     async fn setup(setup: &mut impl e2etest::Setup) -> Option<Self> {
//!         let two = setup.setup::<FixtureTwo>().await?;
//!         Some(Self { number: two.octet as usize * 1024 })
//!     }
//!
//!     async fn teardown(self) { }
//! }
//!
//! e2etest::group!(name = group, fixtures = (FixtureTwo));
//!
//! #[e2etest::test(group = group, timeout = Duration::from_secs(5))]
//! async fn dns_ip_100(one: Arc<FixtureOne>, two: Arc<FixtureTwo>) {
//!     assert_eq!(one.dns_ip, Ipv4Addr::new(127, 0, 100, 1));
//!     assert_eq!(two.octet, 100);
//! }
//!
//! #[e2etest::test(group = group)]
//! async fn dns_ip_200(one: Arc<FixtureOne>, _: Arc<e2etest::Skip>) {
//!     assert_eq!(one.dns_ip, Ipv4Addr::new(127, 0, 200, 1));
//! }
//!
//! #[e2etest::test()]
//! async fn number_and_octet(two: Arc<FixtureTwo>, three: Arc<FixtureThree>) {
//!     assert_eq!(two.octet, 100);
//!     assert_eq!(three.number, 100 * 1024);
//! }
//!
//! }
//!
//! use std::net::Ipv4Addr;
//! use std::time::Duration;
//!
//! tracing_subscriber::fmt::init();
//!
//! if let Some(unshare_info) = e2etest::unshare_info() {
//!     let config = e2etest::ConfigInUnshare::new(unshare_info)
//!             .with_permanent_fixture(sample::FixtureCfg { dns_ip: Ipv4Addr::new(127, 0, 100, 1) })
//!             .with_default_timeout(Duration::from_secs(10))
//!             .with_concurrency(10);
//!     e2etest::run_in_unshare(config);
//!     return;
//! }
//!
//! let config = e2etest::Config::default()
//!     .with_concurrency(10);
//!
//! let stats = e2etest::run(config);
//!
//! assert!(stats.is_success());
//! assert_eq!(stats.tests_defined(), 3);
//! assert_eq!(stats.tests_included(), 3);
//! assert_eq!(stats.tests_launched(), 2);
//! assert_eq!(stats.tests_passed(), 2);
//! assert_eq!(stats.tests_skipped(), 1);
//! ```

mod backtrace;
mod filter;
mod fixture;
mod group;
mod run;
mod statistics;
mod task;
mod test;
mod unshare;

use crate::filter::Filter;
use crate::filter::Restricted;
pub use crate::fixture::Fixture;
use crate::fixture::Fixtures;
pub use crate::fixture::Setup;
pub use crate::fixture::Skip;
pub use crate::group::Group;
pub use crate::group::RunGroup;
use crate::statistics::Event;
pub use crate::statistics::Statistics;
pub use crate::test::RunTest;
pub use crate::test::Test;
pub use crate::unshare::UnshareInfo;
pub use crate::unshare::apparmor_profile_command;
pub use crate::unshare::unshare_info;
#[doc(hidden)]
pub use async_backtrace as __async_backtrace;
pub use e2etest_macros::group;
pub use e2etest_macros::test;
use ipc_channel::ipc::IpcSender;
#[doc(hidden)]
pub use linkme as __linkme;
use std::any::Any;
use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::panic;
use std::process::Child;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;
use tokio::runtime::Builder;
use tracing::error;
use tracing::info;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const PROCESS_STATS_TIMEOUT: Duration = Duration::from_secs(1);
const PROCESS_SLEEP_DURATION: Duration = Duration::from_millis(100);

#[linkme::distributed_slice]
pub static E2ETEST_TESTS: [fn() -> Box<dyn RunTest>];

#[linkme::distributed_slice]
pub static E2ETEST_GROUPS: [fn() -> Box<dyn RunGroup>];

/// Configuration for running tests in the first phase.
#[derive(Debug, Clone)]
pub struct Config {
    filters: Vec<String>,
    concurrency: NonZeroUsize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            filters: Vec::new(),
            concurrency: NonZeroUsize::new(1).unwrap(),
        }
    }
}

impl Config {
    /// Add a filter to select which tests to run.
    ///
    /// The filter is matched against the whole, module-qualified test name
    /// (e.g. `my_tests::crud::simple_create`), so use `*crud*` rather than `crud`
    /// for a substring match. The filter can contain wildcards: `*` matches any
    /// sequence of characters, `?` matches any single character. If several
    /// filters are added, a test runs when it matches any of them.
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filters.push(filter.into());
        self
    }

    /// Set the maximum number of namespaces to run concurrently.
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = NonZeroUsize::new(concurrency).unwrap_or(NonZeroUsize::new(1).unwrap());
        self
    }
}

/// Configuration for running tests in an unshare (second) phase.
#[derive(Debug, Clone)]
pub struct ConfigInUnshare {
    permanent_fixtures: Vec<Arc<dyn Any + Send + Sync>>,
    default_timeout: Duration,
    concurrency: NonZeroUsize,
    unshare_info: UnshareInfo,
}

impl ConfigInUnshare {
    pub fn new(unshare_info: UnshareInfo) -> Self {
        Self {
            permanent_fixtures: Vec::new(),
            default_timeout: DEFAULT_TIMEOUT,
            concurrency: NonZeroUsize::new(1).unwrap(),
            unshare_info,
        }
    }

    /// Add a permanent fixture that will be available for all tests.
    pub fn with_permanent_fixture(mut self, fixture: impl Any + Send + Sync) -> Self {
        self.permanent_fixtures.push(Arc::new(fixture));
        self
    }

    /// Set the default timeout for tests that don't specify one.
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Set the maximum number of tests to run concurrently.
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = NonZeroUsize::new(concurrency).unwrap_or(NonZeroUsize::new(1).unwrap());
        self
    }
}

/// Configuration for running tests without unshare namespaces.
#[derive(Debug, Clone)]
pub struct ConfigWithoutUnshare {
    permanent_fixtures: Vec<Arc<dyn Any + Send + Sync>>,
    filters: Vec<String>,
    default_timeout: Duration,
    concurrency: NonZeroUsize,
}

impl Default for ConfigWithoutUnshare {
    fn default() -> Self {
        Self {
            permanent_fixtures: Vec::new(),
            filters: Vec::new(),
            default_timeout: DEFAULT_TIMEOUT,
            concurrency: NonZeroUsize::new(1).unwrap(),
        }
    }
}

impl ConfigWithoutUnshare {
    /// Add a filter to select which tests to run.
    ///
    /// The filter syntax is the same as for `Config::with_filter()` method
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filters.push(filter.into());
        self
    }

    /// Add a permanent fixture that will be available for all tests.
    pub fn with_permanent_fixture(mut self, fixture: impl Any + Send + Sync) -> Self {
        self.permanent_fixtures.push(Arc::new(fixture));
        self
    }

    /// Set the default timeout for tests that don't specify one.
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Set the maximum number of tests to run concurrently.
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = NonZeroUsize::new(concurrency).unwrap_or(NonZeroUsize::new(1).unwrap());
        self
    }
}

struct Root;

impl Group for Root {
    type Fixture = ();

    fn name(&self) -> &str {
        Self::NAME
    }

    fn tests(&self) -> &[Box<dyn RunTest>] {
        static TESTS: LazyLock<Vec<Box<dyn RunTest>>> =
            LazyLock::new(|| E2ETEST_TESTS.iter().map(|test_fn| test_fn()).collect());
        TESTS.as_slice()
    }
}

trait RootGroup: Group {
    fn groups(&self) -> &[Box<dyn RunGroup>];

    /// Returns an iterator over the (group_name, test_name) for every defined test.
    fn group_test_names(&self) -> impl Iterator<Item = (&str, &str)> {
        self.groups()
            .iter()
            .flat_map(|group| {
                group
                    .test_names()
                    .map(|test_name| (group.name(), test_name))
            })
            .chain(self.tests().iter().map(|test| ("", test.name())))
    }
}

impl RootGroup for Root {
    fn groups(&self) -> &[Box<dyn RunGroup>] {
        static GROUPS: LazyLock<Vec<Box<dyn RunGroup>>> =
            LazyLock::new(|| E2ETEST_GROUPS.iter().map(|group_fn| group_fn()).collect());
        GROUPS.as_slice()
    }
}

impl Root {
    const NAME: &'static str = "";

    fn groups_tests_for_namespaces(&self, filter: &Filter) -> Vec<Restricted> {
        self.group_test_names()
            .filter(|(_, test_name)| filter.consider_test(test_name))
            .map(|(group_name, test_name)| {
                if group_name == Self::NAME {
                    Restricted::Test(test_name.to_string())
                } else {
                    Restricted::Group(group_name.to_string())
                }
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Main entry point for running all tests within unshare environment in a separate process.
///
/// It takes `Config` argument. Returns `Statistics` about the test run.
pub fn run(config: Config) -> Statistics {
    panic::set_hook(Box::new(|info| {
        error!("{info}");
    }));

    if unshare_info().is_some() {
        panic!("run() should be called outside of unshare, but was called inside unshare");
    }

    let root = Root;
    let filter = Filter::new(&Restricted::None, &config.filters, &root);
    let groups_tests = root.groups_tests_for_namespaces(&filter);
    if groups_tests.is_empty() {
        error!("No tests to run");
        return Statistics::new();
    }

    let mut in_progress: Vec<(String, Child, Receiver<Statistics>)> = vec![];
    let mut final_stats = Statistics::new();
    let process_stats = Statistics::new();

    let mut handle_work_done = |in_progress: &mut Vec<(String, Child, Receiver<Statistics>)>| {
        in_progress
            .extract_if(.., |(_, child, _)| child.try_wait().unwrap().is_some())
            .map(|(name, mut child, stats)| {
                if !child.wait().unwrap().success() {
                    error!("child process {name} exited with error");
                    process_stats.record(&name, Event::ProcessFailed);
                    (name, stats, false)
                } else {
                    (name, stats, true)
                }
            })
            .filter_map(|(name, stats, success)| {
                stats
                    .recv_timeout(PROCESS_STATS_TIMEOUT)
                    .inspect_err(|_| {
                        error!("failed to receive statistics from process {name}");
                        if success {
                            process_stats.record(&name, Event::ProcessFailed);
                        }
                    })
                    .ok()
                    .inspect(|_| {
                        if success {
                            process_stats.record(name, Event::ProcessPassed)
                        }
                    })
            })
            .for_each(|stats| {
                final_stats += stats;
            });
    };

    groups_tests.into_iter().for_each(|restricted| {
        loop {
            handle_work_done(&mut in_progress);
            if in_progress.len() < config.concurrency.get() {
                break;
            }
            thread::sleep(PROCESS_SLEEP_DURATION);
        }
        let process_name = restricted.name().to_string();
        process_stats.record(&process_name, Event::ProcessLaunched);
        let (child, ipc_server) = unshare::spawn(restricted, config.filters.clone());
        let (tx, rx) = mpsc::channel();
        thread::spawn({
            move || {
                let (_, stats) = ipc_server.accept().unwrap();
                _ = tx.send(stats);
            }
        });
        in_progress.push((process_name, child, rx));
    });

    while !in_progress.is_empty() {
        handle_work_done(&mut in_progress);
        thread::sleep(PROCESS_SLEEP_DURATION);
    }

    final_stats += process_stats;

    if final_stats.is_success() {
        info!("test run ok: {final_stats:?}");
    } else {
        error!("test run failed: {final_stats:?}");
    }
    final_stats
}

/// Main entry point for running tests in unshare environment.
pub fn run_in_unshare(config: ConfigInUnshare) {
    panic::set_hook(Box::new(|info| {
        error!("{info}");
    }));

    unshare::init_namespaces();

    let tx = IpcSender::connect(config.unshare_info.ipc_channel.clone()).unwrap();

    let stats = run_internal(config);

    tx.send(stats).unwrap();
}

/// Main entry point for running tests without unshare environment.
pub fn run_without_unshare(config: ConfigWithoutUnshare) -> Statistics {
    run_internal(ConfigInUnshare {
        permanent_fixtures: config.permanent_fixtures,
        default_timeout: config.default_timeout,
        concurrency: config.concurrency,
        unshare_info: UnshareInfo {
            restricted: Restricted::None,
            filters: config.filters,
            ipc_channel: String::new(),
        },
    })
}

fn run_internal(config: ConfigInUnshare) -> Statistics {
    let root = Root;
    let filter = Filter::new(
        &config.unshare_info.restricted,
        &config.unshare_info.filters,
        &root,
    );
    let fixtures = Fixtures::with_permanent(config.permanent_fixtures.into_iter());

    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run::run(
            fixtures,
            &root,
            filter,
            config.default_timeout,
            config.concurrency,
        ))
}

/// Returns a list of all group names defined in the test suite.
pub fn group_names() -> Vec<String> {
    Root.group_test_names()
        .filter(|&(group_name, _)| group_name != Group::name(&Root))
        .map(|(group_name, _)| group_name.into())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Returns a list of all test names defined in the test suite.
pub fn test_names() -> Vec<String> {
    Root.group_test_names()
        .map(|(_, test_name)| test_name.into())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadMe;
