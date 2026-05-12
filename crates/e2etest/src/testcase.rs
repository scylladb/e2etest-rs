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
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time;
use tracing::Instrument;
use tracing::Span;
use tracing::error;
use tracing::error_span;
use tracing::info;
use tracing::warn;

tokio::task_local! {
    static XFAIL: Arc<AtomicBool>;
}

/// Mark the current test as expected to fail from this point on.
pub fn xfail() {
    if XFAIL
        .try_with(|f| f.store(true, Ordering::Relaxed))
        .is_err()
    {
        error!("xfail() called outside of a test run - ignoring");
    }
}

type TestFuture = BoxFuture<'static, ()>;

type TestFn<F> = Box<dyn Fn(F) -> TestFuture>;

#[derive(Debug, Default)]
pub(crate) struct RunReport {
    failed_tests: Vec<String>,
    xpassed_tests: Vec<String>,
}

impl RunReport {
    pub(crate) fn failed_tests_summary(&self) -> Option<String> {
        format_test_list("Failed tests:", &self.failed_tests)
    }

    pub(crate) fn xpassed_tests_summary(&self) -> Option<String> {
        format_test_list("Xpassed tests:", &self.xpassed_tests)
    }

    pub(crate) fn is_success(&self) -> bool {
        self.failed_tests.is_empty()
    }
}

impl From<Statistics> for RunReport {
    fn from(stats: Statistics) -> Self {
        Self {
            failed_tests: stats.failed_tests,
            xpassed_tests: stats.xpassed_tests,
        }
    }
}

fn format_test_list(header: &str, tests: &[String]) -> Option<String> {
    if tests.is_empty() {
        return None;
    }

    let items = tests
        .iter()
        .map(|t| format!("- {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("{header}\n{items}"))
}

#[derive(Debug)]
/// Statistics for a test run, including total tests, launched, successful, and failed.
pub(crate) struct Statistics {
    total: usize,
    launched: usize,
    ok: usize,
    failed: usize,
    failed_tests: Vec<String>,
    xfailed: usize,
    xpassed: usize,
    xpassed_tests: Vec<String>,
}

impl Statistics {
    fn new(total: usize) -> Self {
        Self {
            total,
            launched: 0,
            ok: 0,
            failed: 0,
            failed_tests: vec![],
            xfailed: 0,
            xpassed: 0,
            xpassed_tests: vec![],
        }
    }

    fn append(&mut self, other: &Self) {
        self.total += other.total;
        self.launched += other.launched;
        self.ok += other.ok;
        self.failed += other.failed;
        self.failed_tests.extend(other.failed_tests.iter().cloned());
        self.xfailed += other.xfailed;
        self.xpassed += other.xpassed;
        self.xpassed_tests
            .extend(other.xpassed_tests.iter().cloned());
    }

    fn record_failure(&mut self, failed_test: impl Into<String>) {
        self.failed += 1;
        self.failed_tests.push(failed_test.into());
    }

    fn record_xfailed(&mut self) {
        self.xfailed += 1;
    }

    fn record_xpassed(&mut self, name: impl Into<String>) {
        self.xpassed += 1;
        self.xpassed_tests.push(name.into());
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
            match run_single(
                error_span!("init"),
                *timeout,
                init(fixture.clone()),
                backtrace.clone(),
            )
            .await
            {
                TestOutcome::Ok => stats.ok += 1,
                TestOutcome::Failed => {
                    stats.record_failure(format!("{test_case_name}::init"));
                    return stats;
                }
                TestOutcome::Xfailed => {
                    stats.record_xfailed();
                    return stats;
                }
                TestOutcome::Xpassed => stats.record_xpassed(format!("{test_case_name}::init")),
            }
        }

        stream::iter(self.tests.iter())
            .filter(|(name, _, _)| {
                future::ready(test_cases.is_empty() || test_cases.contains(name))
            })
            .then(|(name, timeout, test)| {
                let actors = fixture.clone();
                let backtrace = backtrace.clone();
                async move {
                    let outcome =
                        run_single(error_span!("test", name), *timeout, test(actors), backtrace)
                            .await;
                    (name, outcome)
                }
            })
            .for_each(|(name, outcome)| {
                stats.launched += 1;
                match outcome {
                    TestOutcome::Ok => stats.ok += 1,
                    TestOutcome::Failed => {
                        stats.record_failure(format!("{test_case_name}::{name}"));
                    }
                    TestOutcome::Xfailed => {
                        stats.record_xfailed();
                    }
                    TestOutcome::Xpassed => {
                        stats.record_xpassed(format!("{test_case_name}::{name}"));
                    }
                }
                future::ready(())
            })
            .await;

        if let Some((timeout, cleanup)) = &self.cleanup {
            stats.launched += 1;
            match run_single(
                error_span!("cleanup"),
                *timeout,
                cleanup(fixture.clone()),
                backtrace.clone(),
            )
            .await
            {
                TestOutcome::Ok => stats.ok += 1,
                TestOutcome::Failed => {
                    stats.record_failure(format!("{test_case_name}::cleanup"));
                }
                TestOutcome::Xfailed => {
                    stats.record_xfailed();
                }
                TestOutcome::Xpassed => {
                    stats.record_xpassed(format!("{test_case_name}::cleanup"));
                }
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

/// Outcome of a single test execution.
#[derive(Debug)]
enum TestOutcome {
    Ok,
    Failed,
    Xfailed,
    Xpassed,
}

#[framed]
/// Runs a single test with a timeout, logging the result in the provided span.
async fn run_single(
    span: Span,
    timeout: Duration,
    future: TestFuture,
    backtrace: Backtrace,
) -> TestOutcome {
    let xfail_flag = Arc::new(AtomicBool::new(false));
    let xfail_flag_clone = xfail_flag.clone();
    let task = tokio::spawn(frame!(
        XFAIL
            .scope(xfail_flag_clone, async move {
                time::timeout(timeout, future)
                    .await
                    .expect("test timed out");
            })
            .instrument(span.clone())
    ));
    let xfail_set = || xfail_flag.load(Ordering::Relaxed);
    match task.await {
        Ok(()) => {
            if xfail_set() {
                warn!(parent: &span, "test xpassed (expected failure, but passed)");
                TestOutcome::Xpassed
            } else {
                info!(parent: &span, "test ok");
                TestOutcome::Ok
            }
        }
        Err(err) => {
            if xfail_set() {
                info!(parent: &span, "test xfailed (expected): {err}");
                TestOutcome::Xfailed
            } else {
                let backtrace = backtrace.get();
                error!(parent: &span, "test failed: {err}\n{backtrace}");
                TestOutcome::Failed
            }
        }
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
            format_test_list("Failed tests:", &failed_tests).as_deref(),
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

    #[tokio::test]
    async fn xfail_only_applies_after_call() {
        let timeout = Duration::from_secs(1);
        let test_case = TestCase::empty()
            .with_test("panics_without_xfail", timeout, |_| async {
                panic!("real bug");
                #[allow(unreachable_code)]
                xfail(); // too late - never reached
            })
            .with_test("panics_after_xfail", timeout, |_| async {
                xfail();
                panic!("known bug");
            });

        let stats = test_case
            .run((), "suite", &HashSet::new(), Backtrace::new())
            .await;

        assert_eq!(stats.failed, 1);
        assert_eq!(stats.xfailed, 1);
        assert_eq!(stats.ok, 0);
        assert_eq!(stats.xpassed, 0);
    }

    #[tokio::test]
    async fn xfail_test_that_passes_is_xpassed() {
        let timeout = Duration::from_secs(1);
        let test_case = TestCase::empty().with_test("surprise", timeout, |_| async {
            xfail();
            // does not panic - unexpected success
        });

        let stats = test_case
            .run((), "suite", &HashSet::new(), Backtrace::new())
            .await;

        assert_eq!(stats.ok, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.xfailed, 0);
        assert_eq!(stats.xpassed, 1);
        assert_eq!(stats.xpassed_tests, &["suite::surprise".to_string()]);
    }

    #[tokio::test]
    async fn xfailed_init_aborts_test_case() {
        let timeout = Duration::from_secs(1);
        let test_case = TestCase::empty()
            .with_init(timeout, |_| async {
                xfail();
                panic!("known init bug");
            })
            .with_test("should_not_run", timeout, |_| async {
                panic!("tests must not run after xfailed init");
            })
            .with_cleanup(timeout, |_| async {
                panic!("cleanup must not run after xfailed init");
            });

        let stats = test_case
            .run((), "suite", &HashSet::new(), Backtrace::new())
            .await;

        assert_eq!(stats.xfailed, 1);
        assert_eq!(stats.ok, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.xpassed, 0);
    }

    #[tokio::test]
    async fn xpassed_init_continues_test_case() {
        let timeout = Duration::from_secs(1);
        let test_case = TestCase::empty()
            .with_init(timeout, |_| async {
                xfail();
                // does not panic - unexpected success
            })
            .with_test("should_run", timeout, |_| async {});

        let stats = test_case
            .run((), "suite", &HashSet::new(), Backtrace::new())
            .await;

        assert_eq!(stats.xpassed, 1);
        assert_eq!(stats.ok, 1);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.xfailed, 0);
    }
}
