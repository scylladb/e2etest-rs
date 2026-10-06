/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use crate::fixture::Fixture;
use crate::run::RunContext;
use crate::statistics::Task;
use crate::task;
use crate::test::RunTest;
use async_backtrace::framed;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream;
use tracing::Instrument;
use tracing::error_span;

/// A group of tests.
///
/// Groups can contain other groups and tests. Each group has a fixture that is
/// setup before any tests are run and torn down after all tests are run. Groups are useful
/// for grouping related tests together and sharing setup and teardown logic.
pub trait Group {
    /// The fixture type for this group.
    type Fixture: Fixture;

    /// The name of the group.
    fn name(&self) -> &str;

    /// The tests in this group.
    fn tests(&self) -> &[Box<dyn RunTest>] {
        &[]
    }
}

/// A supporting trait to collecting Group trait objects and running them.
///
/// This is used to run tests or groups recursively and collect statistics.
pub trait RunGroup: Send + Sync + 'static {
    /// The name of the group.
    fn name(&self) -> &str;

    /// Returns true if the group has no tests or subgroups.
    fn is_empty(&self) -> bool;

    /// The names of all tests in this group.
    fn test_names(&self) -> Box<dyn Iterator<Item = &str> + '_>;

    /// Run the group and return statistics about the run.
    fn run_group(&self, ctx: RunContext) -> BoxFuture<'_, ()>;
}

impl<F, G> RunGroup for G
where
    F: Fixture,
    G: Group<Fixture = F>,
    G: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        self.name()
    }

    fn is_empty(&self) -> bool {
        self.tests().is_empty()
    }

    fn test_names(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.tests().iter().map(|test| test.name()))
    }

    #[framed]
    fn run_group(&self, ctx: RunContext) -> BoxFuture<'_, ()> {
        Box::pin(
            async move {
                // Setup the fixture. If it fails, we skip the tests
                let fixture = task::setup(
                    self.name(),
                    Task::Group,
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

                // Run concurrent tests
                stream::iter(
                    self.tests()
                        .iter()
                        .filter(|test| ctx.filter.consider_test(self.name(), test.name())),
                )
                .for_each_concurrent(Some(ctx.concurrency), |test| async {
                    _ = test.run_test(ctx.clone()).await;
                })
                .await;

                // Drop the fixture as it is no longer needed.
                drop(fixture);

                // Teardown group fixture
                task::teardown(
                    self.name(),
                    ctx.fixtures.teardown(),
                    F::timeout_teardown().unwrap_or(ctx.default_timeout),
                    ctx.clone(),
                )
                .await;
            }
            .instrument(error_span!("group", "{}", self.name())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RootGroup;
    use crate::filter::Filter;
    use crate::fixture::Setup;
    use crate::statistics::Event;
    use crate::test::Test;
    use std::sync::Arc;
    use std::time::Duration;

    #[derive(Clone)]
    struct GroupFixture;

    impl Fixture for GroupFixture {
        async fn setup(_: &mut impl Setup) -> Option<Self> {
            Some(Self)
        }
        async fn teardown(self) {
            panic!("cleanup")
        }
    }

    #[derive(Clone)]
    struct TestFixture;

    impl From<&GroupFixture> for TestFixture {
        fn from(_: &GroupFixture) -> Self {
            Self
        }
    }

    impl Fixture for TestFixture {
        async fn setup(_: &mut impl Setup) -> Option<Self> {
            Some(Self)
        }
        async fn teardown(self) {}
    }

    struct TestImpl(String);

    impl Test for TestImpl {
        type Fixture = TestFixture;

        fn name(&self) -> &str {
            &self.0
        }

        fn run(&self, _: Arc<Self::Fixture>) -> impl Future<Output = ()> + Send + 'static {
            let name = self.0.clone();
            async move {
                if name == "crud::boom" {
                    panic!("boom");
                }
            }
        }
    }

    struct GroupImpl {
        name: String,
        tests: Vec<Box<dyn RunTest>>,
    }

    impl Group for GroupImpl {
        type Fixture = GroupFixture;

        fn name(&self) -> &str {
            &self.name
        }

        fn tests(&self) -> &[Box<dyn RunTest>] {
            &self.tests
        }
    }

    impl RootGroup for GroupImpl {
        fn groups(&self) -> &[Box<dyn RunGroup>] {
            &[]
        }
    }

    #[tokio::test]
    async fn collects_failed_test_names() {
        let group = GroupImpl {
            name: "crud".to_string(),
            tests: vec![
                Box::new(TestImpl("crud::ok".to_string())),
                Box::new(TestImpl("crud::boom".to_string())),
            ],
        };
        let ctx = RunContext::new()
            .with_filter(Filter::new(&[""; 0], &group))
            .with_default_timeout(Duration::from_secs(1));

        ctx.statistics.record(
            "crud::ok",
            Event::TestDefined {
                group: "crud".to_string(),
            },
        );
        ctx.statistics.record(
            "crud::boom",
            Event::TestDefined {
                group: "crud".to_string(),
            },
        );
        ctx.statistics.record("crud::ok", Event::TestIncluded);
        ctx.statistics.record("crud::boom", Event::TestIncluded);

        group.run_group(ctx.clone()).await;

        assert!(!ctx.statistics.is_success());
        assert_eq!(ctx.statistics.tests_included(), 2);
        assert_eq!(ctx.statistics.tests_launched(), 2);
        assert_eq!(ctx.statistics.tests_passed(), 1);
        assert_eq!(ctx.statistics.tests_failed(), 1);
        assert_eq!(ctx.statistics.tests_skipped_by_fixture_err(), 0);
        assert_eq!(ctx.statistics.teardowns_failed(), 1);
        assert_eq!(
            ctx.statistics.failed_names(),
            &["crud".to_string(), "crud::boom".to_string()]
        );
    }
}
