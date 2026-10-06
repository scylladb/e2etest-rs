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
struct FixtureRoot(Arc<Counter>);

impl Fixture for FixtureRoot {
    async fn setup(setup: &mut impl Setup) -> Option<Self> {
        let counter = setup.get::<Counter>().await.unwrap();
        counter.0.fetch_add(1, Ordering::Relaxed);
        Some(Self(counter))
    }
    async fn teardown(self) {
        self.0.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct FixtureGroup(Arc<Counter>);

impl Fixture for FixtureGroup {
    async fn setup(setup: &mut impl Setup) -> Option<Self> {
        let counter = setup.get::<Counter>().await.unwrap();
        counter.0.fetch_add(1, Ordering::Relaxed);
        Some(Self(counter))
    }
    async fn teardown(self) {
        self.0.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct FixtureTest(Arc<Counter>);

impl Fixture for FixtureTest {
    async fn setup(setup: &mut impl Setup) -> Option<Self> {
        let counter = setup.get::<Counter>().await.unwrap();
        counter.0.fetch_add(1, Ordering::Relaxed);
        Some(Self(counter))
    }
    async fn teardown(self) {
        self.0.0.fetch_add(1, Ordering::Relaxed);
    }
}

mod root {
    use super::*;

    e2etest::group!(name = group1, fixtures = (FixtureRoot));
}

mod first {
    use super::*;

    e2etest::group!(name = group2, fixtures = (FixtureGroup));

    #[e2etest::test(group = group2)]
    async fn first1(fixture: Arc<FixtureTest>, _deep: Arc<deep::FixtureDeep>) {
        fixture.0.0.fetch_add(1, Ordering::Relaxed);
    }

    #[e2etest::test(group = group2)]
    async fn first2(fixture: Arc<FixtureTest>, _deep: Arc<deep::FixtureDeep>) {
        fixture.0.0.fetch_add(1, Ordering::Relaxed);
    }

    mod deep {
        use super::*;

        #[derive(Clone)]
        pub(crate) struct FixtureDeep(Arc<Counter>);

        impl Fixture for FixtureDeep {
            async fn setup(setup: &mut impl Setup) -> Option<Self> {
                let counter = setup.get::<Counter>().await.unwrap();
                counter.0.fetch_add(1, Ordering::Relaxed);
                Some(Self(counter))
            }
            async fn teardown(self) {
                self.0.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        e2etest::group!(name = group3, fixtures = (FixtureGroup));

        #[e2etest::test(group = group3)]
        async fn deep1(fixture: Arc<FixtureTest>, _deep: Arc<FixtureDeep>) {
            fixture.0.0.fetch_add(1, Ordering::Relaxed);
        }

        #[e2etest::test(group = group3)]
        async fn deep2(fixture: Arc<FixtureTest>, _deep: Arc<FixtureDeep>) {
            fixture.0.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

mod second {
    use super::*;

    e2etest::group!(name = group4, fixtures = (FixtureGroup));

    #[e2etest::test(group = group4)]
    async fn second1(fixture: Arc<FixtureTest>) {
        fixture.0.0.fetch_add(1, Ordering::Relaxed);
    }

    #[e2etest::test(group = group4)]
    async fn second2(fixture: Arc<FixtureTest>) {
        fixture.0.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn hierarchy_names() {
    assert_eq!(
        &e2etest::group_names(),
        &[
            "hierarchy::first::deep::group3",
            "hierarchy::first::group2",
            "hierarchy::second::group4",
        ]
    );
    assert_eq!(
        &e2etest::test_names(),
        &[
            "hierarchy::first::deep::deep1",
            "hierarchy::first::deep::deep2",
            "hierarchy::first::first1",
            "hierarchy::first::first2",
            "hierarchy::second::second1",
            "hierarchy::second::second2",
        ]
    );
}

#[tokio::test]
async fn hierarchy_run() {
    let counter = Arc::new(AtomicUsize::new(0));

    let stats = e2etest::run(
        Config::default()
            .with_permanent_fixture(Counter(Arc::clone(&counter)))
            .with_default_timeout(Duration::from_secs(1)),
    )
    .await;

    // 4 tests * 2 fixtures * 2 + 2 tests * 1 fixture * 2 + 6 tests + 3 groups * 2
    assert_eq!(counter.load(Ordering::Relaxed), 32);

    assert!(stats.is_success());
    assert_eq!(stats.groups_defined(), 3);
    assert_eq!(stats.groups_included(), 3);
    assert_eq!(stats.tests_defined(), 6);
    assert_eq!(stats.tests_included(), 6);
    assert_eq!(stats.tests_launched(), 6);
    assert_eq!(stats.tests_passed(), 6);
    assert_eq!(stats.tests_failed(), 0);
}
