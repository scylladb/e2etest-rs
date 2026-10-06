/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use e2etest::Config;
use e2etest::Fixture;
use e2etest::Setup;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

struct Counter(Arc<AtomicUsize>);

#[derive(Clone)]
struct FixtureCount(Arc<Counter>);

impl Fixture for FixtureCount {
    async fn setup(setup: &mut impl Setup) -> Option<Self> {
        let counter = setup.get::<Counter>().await.unwrap();
        Some(Self(counter))
    }
    async fn teardown(self) {}
}

#[e2etest::test()]
async fn filter_test1(fixture: Arc<FixtureCount>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

#[e2etest::test()]
async fn filter_test2(fixture: Arc<FixtureCount>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

e2etest::group!(name = filter_group1);

#[e2etest::test(group = filter_group1)]
async fn filter_test1_1(fixture: Arc<FixtureCount>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

#[e2etest::test(group = filter_group1)]
async fn filter_test1_2(fixture: Arc<FixtureCount>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

e2etest::group!(name = filter_group2);

#[e2etest::test(group = filter_group2)]
async fn filter_test2_1(fixture: Arc<FixtureCount>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

#[e2etest::test(group = filter_group2)]
async fn filter_test2_2(fixture: Arc<FixtureCount>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

#[tokio::test]
async fn filter_by_group() {
    let counter = Arc::new(AtomicUsize::new(0));

    let stats = e2etest::run(
        Config::default()
            .with_permanent_fixture(Counter(Arc::clone(&counter)))
            .with_filter("group1::")
            .with_default_timeout(Duration::from_secs(1)),
    )
    .await;

    // 2 tests
    assert_eq!(counter.load(Ordering::Relaxed), 2);

    assert!(stats.is_success());
    assert_eq!(stats.groups_defined(), 2);
    assert_eq!(stats.groups_included(), 1);
    assert_eq!(stats.tests_defined(), 6);
    assert_eq!(stats.tests_included(), 2);
    assert_eq!(stats.tests_launched(), 2);
    assert_eq!(stats.tests_passed(), 2);
}

#[tokio::test]
async fn filter_by_test() {
    let counter = Arc::new(AtomicUsize::new(0));

    let stats = e2etest::run(
        Config::default()
            .with_permanent_fixture(Counter(Arc::clone(&counter)))
            .with_filter("::test2_")
            .with_default_timeout(Duration::from_secs(1)),
    )
    .await;

    // 2 tests
    assert_eq!(counter.load(Ordering::Relaxed), 2);

    assert!(stats.is_success());
    assert_eq!(stats.tests_defined(), 6);
    assert_eq!(stats.tests_included(), 2);
    assert_eq!(stats.tests_launched(), 2);
    assert_eq!(stats.tests_passed(), 2);
}

#[test]
fn test_and_group_names() {
    assert_eq!(
        e2etest::group_names(),
        ["filter::filter_group1", "filter::filter_group2"]
    );
    assert_eq!(
        e2etest::test_names(),
        [
            "filter::filter_test1",
            "filter::filter_test1_1",
            "filter::filter_test1_2",
            "filter::filter_test2",
            "filter::filter_test2_1",
            "filter::filter_test2_2"
        ]
    );
}
