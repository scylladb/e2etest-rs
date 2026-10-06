/*
 * Copyright 2025-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

//! This library provides a framework for defining and running End-to-End tests on network service
//! for Rust. It allows users to define test cases with multiple tests, and provides a global
//! fixture for all of them.
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
//! tokio::runtime::Runtime::new().unwrap().block_on(async move {
//!     use std::net::Ipv4Addr;
//!     use std::time::Duration;
//!
//!     let config = e2etest::Config::default()
//!         .with_permanent_fixture(sample::FixtureCfg { dns_ip: Ipv4Addr::new(127, 0, 100, 1) })
//!         .with_default_timeout(Duration::from_secs(10));
//!     let stats = e2etest::run(config).await;
//!     assert!(stats.is_success());
//!     assert_eq!(stats.tests_defined(), 3);
//!     assert_eq!(stats.tests_included(), 3);
//!     assert_eq!(stats.tests_launched(), 2);
//!     assert_eq!(stats.tests_passed(), 2);
//!     assert_eq!(stats.tests_skipped(), 1);
//! });
//! ```

mod backtrace;
mod filter;
mod fixture;
mod group;
mod run;
mod statistics;
mod task;
mod test;

use crate::filter::Filter;
pub use crate::fixture::Fixture;
use crate::fixture::Fixtures;
pub use crate::fixture::Setup;
pub use crate::fixture::Skip;
pub use crate::group::Group;
pub use crate::group::RunGroup;
pub use crate::statistics::Statistics;
pub use crate::test::RunTest;
pub use crate::test::Test;
#[doc(hidden)]
pub use async_backtrace as __async_backtrace;
use async_backtrace::framed;
pub use e2etest_macros::group;
pub use e2etest_macros::test;
#[doc(hidden)]
pub use linkme as __linkme;
use std::any::Any;
use std::collections::BTreeSet;
use std::panic;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;
use tracing::error;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[linkme::distributed_slice]
pub static E2ETEST_TESTS: [fn() -> Box<dyn RunTest>];

#[linkme::distributed_slice]
pub static E2ETEST_GROUPS: [fn() -> Box<dyn RunGroup>];

/// Configuration for running tests.
pub struct Config {
    permanent_fixtures: Vec<Arc<dyn Any + Send + Sync>>,
    filters: Vec<String>,
    default_timeout: Duration,
    concurrency: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            permanent_fixtures: Vec::new(),
            filters: Vec::new(),
            default_timeout: DEFAULT_TIMEOUT,
            concurrency: 1,
        }
    }
}

impl Config {
    /// Add a permanent fixture that will be available for all tests.
    pub fn with_permanent_fixture(mut self, fixture: impl Any + Send + Sync) -> Self {
        self.permanent_fixtures.push(Arc::new(fixture));
        self
    }

    /// Add a filter to select which tests to run.
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filters.push(filter.into());
        self
    }

    /// Set the default timeout for tests that don't specify one.
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Set the maximum number of tests to run concurrently.
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency;
        self
    }
}

struct Root;

impl Group for Root {
    type Fixture = ();

    fn name(&self) -> &str {
        ""
    }

    fn tests(&self) -> &[Box<dyn RunTest>] {
        static TESTS: LazyLock<Vec<Box<dyn RunTest>>> =
            LazyLock::new(|| E2ETEST_TESTS.iter().map(|test_fn| test_fn()).collect());
        TESTS.as_slice()
    }
}

trait RootGroup: Group {
    fn groups(&self) -> &[Box<dyn RunGroup>];

    /// Returns an iterator over the (group_name, test_name) of all tests in this group and its subgroups.
    fn group_test_names(&self) -> impl Iterator<Item = (&str, &str)> {
        self.tests()
            .iter()
            .map(|test| ("", test.name()))
            .chain(self.groups().iter().flat_map(|subgroup| {
                subgroup
                    .test_names()
                    .map(|test_name| (subgroup.name(), test_name))
            }))
    }
}

impl RootGroup for Root {
    fn groups(&self) -> &[Box<dyn RunGroup>] {
        static GROUPS: LazyLock<Vec<Box<dyn RunGroup>>> =
            LazyLock::new(|| E2ETEST_GROUPS.iter().map(|group_fn| group_fn()).collect());
        GROUPS.as_slice()
    }
}

/// Main entry point for running tests.
///
/// It takes `Config` argument. Returns `Statistics` about the test run.
#[framed]
pub async fn run(config: Config) -> Statistics {
    panic::set_hook(Box::new(|info| {
        error!("{info}");
    }));

    let fixtures = Fixtures::with_permanent(config.permanent_fixtures.into_iter());
    let root = Root;
    let filter = Filter::new(&config.filters, &root);

    run::run(
        fixtures,
        &root,
        filter,
        config.default_timeout,
        config.concurrency,
    )
    .await
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
