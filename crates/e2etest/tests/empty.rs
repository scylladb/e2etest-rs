/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

mod common;

use e2etest::ConfigWithoutUnshare;

e2etest::group!(name = empty_group);

#[test]
fn empty() {
    common::enable_tracing();

    let stats = e2etest::run_without_unshare(ConfigWithoutUnshare::default());

    assert!(!stats.is_success());
    assert_eq!(stats.tests_defined(), 0);
}
