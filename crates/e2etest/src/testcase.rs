/*
 * Copyright 2025-present ScyllaDB
 * SPDX-License-Identifier: LicenseRef-ScyllaDB-Source-Available-1.0
 */

use async_backtrace::frame;
use async_backtrace::framed;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream;
use futures::stream::StreamExt;
use std::collections::HashMap;
use std::collections::HashSet;
use std::future;
use std::panic;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;
use tokio::time;
use tracing::Instrument;
use tracing::Span;
use tracing::error;
use tracing::error_span;
use tracing::info;

type TestFuture = BoxFuture<'static, ()>;

type TestFn<F> = Box<dyn Fn(F) -> TestFuture>;

#[derive(Debug, Default)]
pub(crate) struct RunReport {
    failed_tests: Vec<String>,
}

impl RunReport {
    pub(crate) fn failed_tests_summary(&self) -> Option<String> {
        format_failed_tests(&self.failed_tests)
    }

    pub(crate) fn is_success(&self) -> bool {
        self.failed_tests.is_empty()
    }
}

impl From<Statistics> for RunReport {
    fn from(stats: Statistics) -> Self {
        Self {
            failed_tests: stats.failed_tests,
        }
    }
}

pub(crate) fn format_failed_tests(failed_tests: &[String]) -> Option<String> {
    if failed_tests.is_empty() {
        return None;
    }

    let failed_tests = failed_tests
        .iter()
        .map(|failed_test| format!("- {failed_test}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("Failed tests:\n{failed_tests}"))
}

#[derive(Debug)]
/// Statistics for a test run, including total tests, launched, successful, and failed.
pub(crate) struct Statistics {
    total: usize,
    launched: usize,
    ok: usize,
    failed: usize,
    failed_tests: Vec<String>,
}

impl Statistics {
    fn new(total: usize) -> Self {
        Self {
            total,
            launched: 0,
            ok: 0,
            failed: 0,
            failed_tests: vec![],
        }
    }

    fn append(&mut self, other: &Self) {
        self.total += other.total;
        self.launched += other.launched;
        self.ok += other.ok;
        self.failed += other.failed;
        self.failed_tests.extend(other.failed_tests.iter().cloned());
    }

    fn record_failure(&mut self, failed_test: impl Into<String>) {
        self.failed += 1;
        self.failed_tests.push(failed_test.into());
    }
}

/// Represents a single test case, which can include initialization, multiple tests, and cleanup.
pub struct TestCase<F>
where
    F: Clone + Send + Sync + 'static,
{
    init: Option<(Duration, TestFn<F>)>,
    tests: Vec<(String, Duration, TestFn<F>)>,
    cleanup: Option<(Duration, TestFn<F>)>,
}

impl<F> TestCase<F>
where
    F: Clone + Send + Sync + 'static,
{
    /// Creates a new empty test case.
    pub fn empty() -> Self {
        Self {
            init: None,
            tests: vec![],
            cleanup: None,
        }
    }

    /// Returns a reference to the tests in this test case.
    pub fn tests(&self) -> &Vec<(String, Duration, TestFn<F>)> {
        &self.tests
    }

    /// Add an initialization function to the test case.
    pub fn with_init<TF, R>(mut self, timeout: Duration, test_fn: TF) -> Self
    where
        TF: Fn(F) -> R + 'static,
        R: Future<Output = ()> + Send + 'static,
    {
        self.init = Some((timeout, wrap_test_fn(test_fn)));
        self
    }

    /// Add a test to the test case.
    pub fn with_test<TF, R>(mut self, name: impl ToString, timeout: Duration, test_fn: TF) -> Self
    where
        TF: Fn(F) -> R + 'static,
        R: Future<Output = ()> + Send + 'static,
    {
        self.tests
            .push((name.to_string(), timeout, wrap_test_fn(test_fn)));
        self
    }

    /// Add a cleanup function to the test case.
    pub fn with_cleanup<TF, R>(mut self, timeout: Duration, test_fn: TF) -> Self
    where
        TF: Fn(F) -> R + 'static,
        R: Future<Output = ()> + Send + 'static,
    {
        self.cleanup = Some((timeout, wrap_test_fn(test_fn)));
        self
    }

    /// Add a sub-module registration function to the test case.
    pub fn with_module(self, register: impl FnOnce(Self) -> Self) -> Self {
        register(self)
    }

    #[framed]
    /// Run initialization, all tests, and cleanup functions in the test case.
    async fn run(
        &self,
        fixture: F,
        test_case_name: &str,
        test_cases: &HashSet<String>,
        backtrace: Backtrace,
    ) -> Statistics {
        let total = if test_cases.is_empty() {
            // Run all tests
            self.tests.len()
        } else {
            test_cases.len()
        } + (self.init.is_some() as usize)
            + (self.cleanup.is_some() as usize);
        let mut stats = Statistics::new(total);

        if let Some((timeout, init)) = &self.init {
            stats.launched += 1;
            if !run_single(
                error_span!("init"),
                *timeout,
                init(fixture.clone()),
                backtrace.clone(),
            )
            .await
            {
                stats.record_failure(format!("{test_case_name}::init"));
                return stats;
            }
            stats.ok += 1;
        }

        stream::iter(self.tests.iter())
            .filter(|(name, _, _)| {
                future::ready(test_cases.is_empty() || test_cases.contains(name))
            })
            .then(|(name, timeout, test)| {
                let actors = fixture.clone();
                let backtrace = backtrace.clone();
                async move {
                    let ok =
                        run_single(error_span!("test", name), *timeout, test(actors), backtrace)
                            .await;
                    (name, ok)
                }
            })
            .for_each(|(name, ok)| {
                stats.launched += 1;
                if ok {
                    stats.ok += 1;
                } else {
                    stats.record_failure(format!("{test_case_name}::{name}"));
                }
                future::ready(())
            })
            .await;

        if let Some((timeout, cleanup)) = &self.cleanup {
            stats.launched += 1;
            if !run_single(
                error_span!("cleanup"),
                *timeout,
                cleanup(fixture.clone()),
                backtrace.clone(),
            )
            .await
            {
                stats.record_failure(format!("{test_case_name}::cleanup"));
            } else {
                stats.ok += 1;
            }
        }

        stats
    }
}

/// Wraps a test function into a `TestFn` type, which is a boxed future that can be stored in a
/// container.
fn wrap_test_fn<TF, R, F>(test_fn: TF) -> TestFn<F>
where
    TF: Fn(F) -> R + 'static,
    R: Future<Output = ()> + Send + 'static,
{
    Box::new(move |fixture: F| {
        let future = test_fn(fixture);
        future.boxed()
    })
}

#[framed]
/// Runs a single test with a timeout, logging the result in the provided span.
async fn run_single(
    span: Span,
    timeout: Duration,
    future: TestFuture,
    backtrace: Backtrace,
) -> bool {
    let task = tokio::spawn(frame!(
        async move {
            time::timeout(timeout, future)
                .await
                .expect("test timed out");
        }
        .instrument(span.clone())
    ));
    if let Err(err) = task.await {
        let backtrace = backtrace.get();
        error!(parent: &span, "test failed: {err}\n{backtrace}");
        false
    } else {
        info!(parent: &span, "test ok");
        true
    }
}

#[framed]
/// Runs all test cases, filtering them based on the provided filter map.
pub(crate) async fn run<F>(
    fixture: F,
    test_cases: Vec<(String, TestCase<F>)>,
    filter_map: Arc<HashMap<String, HashSet<String>>>,
) -> RunReport
where
    F: Clone + Send + Sync + 'static,
{
    let backtrace = setup_panic_hook();

    let stats = stream::iter(test_cases.into_iter())
        .filter(|(file_name, _)| {
            let process = filter_map.is_empty() || filter_map.contains_key(file_name);
            async move { process }
        })
        .then(|(name, test_case)| {
            let fixture = fixture.clone();
            let filter = filter_map.clone();
            let file_name = name.clone();
            let backtrace = backtrace.clone();
            async move {
                let stats = test_case
                    .run(
                        fixture,
                        &file_name,
                        filter.get(&file_name).unwrap_or(&HashSet::new()),
                        backtrace,
                    )
                    .instrument(error_span!("test-case", name))
                    .await;
                if stats.failed > 0 {
                    error!("test case failed: {stats:?}");
                } else {
                    info!("test case ok: {stats:?}");
                }
                stats
            }
        })
        .fold(Statistics::new(0), |mut acc, stats| async move {
            acc.append(&stats);
            acc
        })
        .await;

    clear_panic_hook();

    if stats.failed > 0 {
        error!("test run failed: {stats:?}");
        return stats.into();
    }
    info!("test run ok: {stats:?}");
    stats.into()
}

#[derive(Clone, Debug)]
struct Backtrace(Arc<RwLock<String>>);

impl Backtrace {
    fn new() -> Self {
        Self(Arc::new(RwLock::new(String::new())))
    }

    fn get(&self) -> String {
        self.0.read().unwrap().clone()
    }
}

fn setup_panic_hook() -> Backtrace {
    let backtrace = Backtrace::new();
    let old_hook = panic::take_hook();
    panic::set_hook(Box::new({
        let backtrace = backtrace.clone();
        move |info| {
            *backtrace.0.write().unwrap() = async_backtrace::taskdump_tree(true);
            old_hook(info);
        }
    }));
    backtrace
}

fn clear_panic_hook() {
    _ = panic::take_hook();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn formats_failed_tests_as_bulleted_list() {
        let failed_tests = vec!["crud::boom".to_string(), "crud::cleanup".to_string()];

        assert_eq!(
            format_failed_tests(&failed_tests).as_deref(),
            Some("Failed tests:\n- crud::boom\n- crud::cleanup")
        );
    }

    #[tokio::test]
    async fn collects_failed_test_names() {
        let timeout = Duration::from_secs(1);
        let test_case = TestCase::empty()
            .with_test("ok", timeout, |_actors| async {})
            .with_test("boom", timeout, |_actors| async {
                panic!("boom");
            })
            .with_cleanup(timeout, |_actors| async {
                panic!("cleanup");
            });

        let stats = test_case
            .run((), "crud", &HashSet::new(), Backtrace::new())
            .await;

        assert_eq!(stats.failed, 2);
        assert_eq!(
            stats.failed_tests,
            &["crud::boom".to_string(), "crud::cleanup".to_string()]
        );
    }
}
