/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use e2etest::Config;
use e2etest::Setup;
use e2etest::Skip;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

struct Counter(Arc<AtomicUsize>);

#[derive(Clone)]
struct Fixture(Arc<Counter>);

impl e2etest::Fixture for Fixture {
    async fn setup(setup: &mut impl Setup) -> Option<Self> {
        let counter = setup.get::<Counter>().await.unwrap();
        Some(Self(counter))
    }
    async fn teardown(self) {}
}

e2etest::group!(name = skip_group, fixtures = (Skip));

#[e2etest::test()]
async fn first(fixture: Arc<Fixture>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

#[e2etest::test()]
async fn second(fixture: Arc<Fixture>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

#[e2etest::test()]
async fn skipped(fixture: Arc<Fixture>, _: Arc<Skip>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

#[e2etest::test(group = skip_group)]
async fn in_skipped_group(fixture: Arc<Fixture>) {
    fixture.0.0.fetch_add(1, Ordering::Relaxed);
}

#[tokio::test]
async fn skip() {
    let counter = Arc::new(AtomicUsize::new(0));

    let stats = e2etest::run(
        Config::default()
            .with_permanent_fixture(Counter(Arc::clone(&counter)))
            .with_default_timeout(Duration::from_secs(1)),
    )
    .await;

    // 4 tests - 2 test-skipped
    assert_eq!(counter.load(Ordering::Relaxed), 2);

    assert!(stats.is_success());
    assert_eq!(stats.tests_defined(), 4);
    assert_eq!(stats.tests_included(), 4);
    assert_eq!(stats.tests_launched(), 2);
    assert_eq!(stats.tests_passed(), 2);
    assert_eq!(stats.tests_failed(), 0);
    assert_eq!(stats.tests_skipped(), 2);
}
