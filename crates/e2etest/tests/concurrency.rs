/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use e2etest::Config;
use e2etest::Setup;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_millis(1);
const REPEATS: usize = 10;

struct Log(Arc<Mutex<Vec<String>>>);

#[derive(Clone)]
struct Concurrently(Arc<Log>);

impl e2etest::Fixture for Concurrently {
    async fn setup(setup: &mut impl Setup) -> Option<Self> {
        let log = setup.get::<Log>().await.unwrap();
        Some(Self(log))
    }
    async fn teardown(self) {}
}

#[derive(Clone)]
struct NotConcurrently(Arc<Log>);

impl e2etest::Fixture for NotConcurrently {
    async fn setup(setup: &mut impl Setup) -> Option<Self> {
        let log = setup.get::<Log>().await.unwrap();
        Some(Self(log))
    }
    async fn teardown(self) {}
}

e2etest::group!(name = concurrency_group1);

#[e2etest::test(group = concurrency_group1)]
async fn test1(fixture: Arc<Concurrently>) {
    for _ in 0..REPEATS {
        tokio::time::sleep(TIMEOUT).await;
        fixture.0.0.lock().unwrap().push("test1".to_string());
    }
}

#[e2etest::test(group = concurrency_group1)]
async fn test2(fixture: Arc<Concurrently>) {
    for _ in 0..REPEATS {
        tokio::time::sleep(TIMEOUT).await;
        fixture.0.0.lock().unwrap().push("test2".to_string());
    }
}

#[e2etest::test(group = concurrency_group1)]
async fn test3(fixture: Arc<Concurrently>) {
    for _ in 0..REPEATS {
        tokio::time::sleep(TIMEOUT).await;
        fixture.0.0.lock().unwrap().push("test3".to_string());
    }
}

#[e2etest::test()]
async fn test4(fixture: Arc<NotConcurrently>) {
    for _ in 0..REPEATS {
        tokio::time::sleep(TIMEOUT).await;
        fixture.0.0.lock().unwrap().push("test4".to_string());
    }
}

#[e2etest::test()]
async fn test5(fixture: Arc<NotConcurrently>) {
    for _ in 0..REPEATS {
        tokio::time::sleep(TIMEOUT).await;
        fixture.0.0.lock().unwrap().push("test5".to_string());
    }
}

#[tokio::test]
async fn concurrency() {
    let log = Arc::new(Mutex::new(Vec::new()));

    let stats = e2etest::run(
        Config::default()
            .with_permanent_fixture(Log(Arc::clone(&log)))
            .with_default_timeout(Duration::from_secs(1))
            .with_concurrency(10),
    )
    .await;

    let log = log.lock().unwrap();
    let log = log.as_slice();
    assert_ne!(&log[00..10], &["test1"; 10]);
    assert_ne!(&log[00..10], &["test2"; 10]);
    assert_ne!(&log[00..10], &["test3"; 10]);
    assert_ne!(&log[10..20], &["test1"; 10]);
    assert_ne!(&log[10..20], &["test2"; 10]);
    assert_ne!(&log[10..20], &["test3"; 10]);
    assert!(&log[0..30].contains(&"test1".to_string()));
    assert!(&log[0..30].contains(&"test2".to_string()));
    assert!(&log[0..30].contains(&"test3".to_string()));
    assert_eq!(&log[30..40], &["test4"; 10]);
    assert_eq!(&log[40..50], &["test5"; 10]);

    assert!(stats.is_success());
    assert_eq!(stats.tests_defined(), 5);
    assert_eq!(stats.tests_included(), 5);
    assert_eq!(stats.tests_launched(), 5);
    assert_eq!(stats.tests_passed(), 5);
}
