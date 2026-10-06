/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use crate::DEFAULT_TIMEOUT;
use crate::RootGroup;
use crate::backtrace;
use crate::backtrace::Backtrace;
use crate::filter::Filter;
use crate::fixture::Fixtures;
use crate::statistics::Event;
use crate::statistics::Statistics;
use async_backtrace::framed;
use std::fmt::Debug;
use std::time::Duration;
use tracing::Instrument;
use tracing::error;
use tracing::error_span;
use tracing::info;

#[derive(Clone)]
pub struct RunContext {
    pub(crate) fixtures: Fixtures,
    pub(crate) statistics: Statistics,
    pub(crate) backtrace: Backtrace,
    pub(crate) filter: Filter,
    pub(crate) default_timeout: Duration,
    pub(crate) concurrency: usize,
}

impl RunContext {
    pub(crate) fn new() -> Self {
        Self {
            fixtures: Fixtures::new(),
            statistics: Statistics::new(),
            backtrace: Backtrace::new(),
            filter: Filter::empty(),
            default_timeout: DEFAULT_TIMEOUT,
            concurrency: 1,
        }
    }

    pub(crate) fn with_fixtures(mut self, fixtures: Fixtures) -> Self {
        self.fixtures = fixtures;
        self
    }

    pub(crate) fn with_backtrace(mut self, backtrace: Backtrace) -> Self {
        self.backtrace = backtrace;
        self
    }

    pub(crate) fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = filter;
        self
    }

    pub(crate) fn with_default_timeout(mut self, default_timeout: Duration) -> Self {
        self.default_timeout = default_timeout;
        self
    }

    pub(crate) fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = if concurrency > 0 { concurrency } else { 1 };
        self
    }
}

impl Debug for RunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run").finish()
    }
}

#[framed]
/// Runs all test cases, filtering them based on the provided filter map.
pub(crate) async fn run(
    fixtures: Fixtures,
    group: &impl RootGroup,
    filter: Filter,
    default_timeout: Duration,
    concurrency: usize,
) -> Statistics {
    let ctx = RunContext::new()
        .with_fixtures(fixtures)
        .with_filter(filter.clone())
        .with_backtrace(backtrace::setup_panic_hook())
        .with_default_timeout(default_timeout)
        .with_concurrency(concurrency);

    let mut empty = true;
    group
        .group_test_names()
        .inspect(|&(group_name, test_name)| {
            ctx.statistics.record(
                test_name,
                Event::TestDefined {
                    group: group_name.to_string(),
                },
            );
        })
        .filter(|(group_name, test_name)| filter.consider_test(group_name, test_name))
        .for_each(|(_, test_name)| {
            empty = false;
            ctx.statistics.record(test_name, Event::TestIncluded);
        });

    if empty {
        error!("no tests to run");
    } else {
        // Run groups
        for group in group
            .groups()
            .iter()
            .filter(|&group| filter.consider_group(group.name()))
            .filter(|&group| !group.is_empty())
        {
            group
                .run_group(ctx.clone())
                .instrument(error_span!("group", "{}", group.name()))
                .await;
        }

        // Run tests
        for test in group
            .tests()
            .iter()
            .filter(|test| filter.consider_test(group.name(), test.name()))
        {
            test.run_test(ctx.clone())
                .instrument(error_span!("test", "{}", test.name()))
                .await;
        }
    }

    backtrace::clear_panic_hook();

    let stats = ctx.statistics;
    if stats.is_success() {
        info!("test run ok: {stats:?}");
    } else {
        error!("test run failed: {stats:?}");
    }
    stats
}
