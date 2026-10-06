/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use crate::fixture::Fixture;
use crate::run::RunContext;
use crate::statistics::Task;
use crate::task;
use async_backtrace::framed;
use futures::future::BoxFuture;
use std::sync::Arc;
use std::time::Duration;
use tracing::Instrument;
use tracing::error_span;

/// A test that can be run by the test runner.
pub trait Test: Send + Sync + 'static {
    /// The fixture type that this test uses.
    type Fixture: Fixture;

    /// The timeout for the test. If `None`, the default timeout will be used.
    fn timeout(&self) -> Option<Duration> {
        None
    }

    /// The name of the test.
    fn name(&self) -> &str;

    /// Run the test with the given fixture.
    fn run(&self, fixture: Arc<Self::Fixture>) -> impl Future<Output = ()> + Send + 'static;
}

/// A supporting trait to collecting Test trait objects and running them.
///
/// This is used to run test with its fixture and collect statistics.
pub trait RunTest: Send + Sync + 'static {
    /// The name of the test.
    fn name(&self) -> &str;

    /// Run the test with the given fixture and collect statistics.
    fn run_test(&self, ctx: RunContext) -> BoxFuture<'_, ()>;
}

impl<F, T> RunTest for T
where
    F: Fixture,
    T: Test<Fixture = F>,
    T: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        self.name()
    }

    #[framed]
    fn run_test(&self, ctx: RunContext) -> BoxFuture<'_, ()> {
        Box::pin(
            async move {
                // Setup the fixture. If it fails, we skip the test and teardown.
                let fixture = task::setup(
                    self.name(),
                    Task::Test,
                    ctx.fixtures.setup::<F>(),
                    F::timeout_setup().unwrap_or(ctx.default_timeout),
                    ctx.clone(),
                )
                .await;
                let fixture = match fixture {
                    Ok(Some(fixture)) => fixture,
                    Ok(None) | Err(()) => {
                        // Setup could have created other fixtures, so we need to teardown those
                        task::teardown(
                            self.name(),
                            ctx.fixtures.teardown(),
                            F::timeout_teardown().unwrap_or(ctx.default_timeout),
                            ctx.clone(),
                        )
                        .await;
                        return;
                    }
                };

                task::test(
                    self.name(),
                    self.run(fixture.clone()),
                    self.timeout().unwrap_or(ctx.default_timeout),
                    ctx.clone(),
                )
                .await;

                // Drop the fixture as it is no longer needed.
                drop(fixture);

                // Run the teardown
                task::teardown(
                    self.name(),
                    ctx.fixtures.teardown(),
                    F::timeout_teardown().unwrap_or(ctx.default_timeout),
                    ctx.clone(),
                )
                .await;
            }
            .instrument(error_span!("test", "{}", self.name())),
        )
    }
}
