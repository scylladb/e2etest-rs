/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use e2etest::ConfigUnshare;

e2etest::group!(name = empty_group);

#[test]
fn empty() {
    let stats = e2etest::run_without_unshare(ConfigUnshare::default(), vec![]);

    assert!(!stats.is_success());
    assert_eq!(stats.tests_defined(), 0);
}
