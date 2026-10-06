/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use crate::Group;
use crate::Root;
use itertools::Itertools;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::iter;
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Clone)]
/// Statistics for a test run.
pub struct Statistics(Arc<Mutex<Inner>>);

#[derive(Debug, Clone, Copy)]
pub(crate) enum Task {
    Group,
    Test,
}

#[derive(Debug, Clone)]
pub(crate) enum Event {
    SetupLaunched,
    SetupSkipped(Task),
    SetupPassed,
    SetupFailed(Task),

    TeardownLaunched,
    TeardownPassed,
    TeardownFailed,

    TestDefined { group: String },
    TestIncluded,
    TestLaunched,
    TestPassed,
    TestFailed,
}

#[derive(Debug, Clone)]
struct EventEntry {
    name: String,
    event: Event,
}

impl EventEntry {
    fn is_failed(&self) -> bool {
        matches!(
            self.event,
            Event::SetupFailed(_) | Event::TestFailed | Event::TeardownFailed
        )
    }
}

impl Debug for Statistics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Statistics")
            .field("groups_defined", &self.groups_defined())
            .field("groups_included", &self.groups_included())
            .field("tests_defined", &self.tests_defined())
            .field("tests_included", &self.tests_included())
            .field("tests_launched", &self.tests_launched())
            .field("tests_skipped", &self.tests_skipped())
            .field(
                "tests_skipped_by_fixture_err",
                &self.tests_skipped_by_fixture_err(),
            )
            .field("tests_passed", &self.tests_passed())
            .field("tests_failed", &self.tests_failed())
            .field("setups_launched", &self.setups_launched())
            .field("setups_skipped", &self.setups_skipped())
            .field("setups_passed", &self.setups_passed())
            .field("setups_failed", &self.setups_failed())
            .field("teardowns_launched", &self.teardowns_launched())
            .field("teardowns_passed", &self.teardowns_passed())
            .field("teardowns_failed", &self.teardowns_failed())
            .finish()
    }
}

struct Inner {
    defined: HashSet<String>,
    included: HashSet<String>,
    groups: HashMap<String, HashSet<String>>,
    events: Vec<EventEntry>,
}

impl Inner {
    fn new() -> Self {
        Self {
            defined: HashSet::new(),
            included: HashSet::new(),
            groups: HashMap::new(),
            events: Vec::new(),
        }
    }

    /// Returns an iterator over all included tests which are subtests of the given name.
    ///
    /// The `name` parameter is a full path to a test or a group.
    fn iter_included(&self, name: &str) -> impl Iterator<Item = &String> {
        iter::empty().chain(self.included.get(name)).chain(
            self.groups
                .get(name)
                .into_iter()
                .flat_map(|group_tests| group_tests.iter())
                .filter(|&test_name| self.included.contains(test_name)),
        )
    }
}

impl Statistics {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(Inner::new())))
    }

    pub(crate) fn record(&self, name: impl Into<String>, event: Event) {
        let name = name.into();
        let mut inner = self.0.lock().unwrap();
        match event {
            Event::TestDefined { group } => {
                inner.groups.entry(group).or_default().insert(name.clone());
                inner.defined.insert(name);
                return;
            }
            Event::TestIncluded => {
                assert!(
                    inner.defined.contains(&name),
                    "Test {name} was not defined before being included"
                );
                inner.included.insert(name);
                return;
            }
            Event::TestLaunched | Event::TestPassed | Event::TestFailed => {
                assert!(
                    inner.included.contains(&name),
                    "Test {name} was not included before being processed for event {event:?}"
                );
            }
            _ => {
                assert!(
                    inner.iter_included(&name).next().is_some(),
                    "Path {name} was not included before being processed for event {event:?}"
                );
            }
        }
        inner.events.push(EventEntry { name, event });
    }

    /// Returns true if there are no failed tasks
    pub fn is_success(&self) -> bool {
        let inner = self.0.lock().unwrap();
        !inner.included.is_empty() && !inner.events.iter().any(EventEntry::is_failed)
    }

    /// Returns number of total defined groups.
    pub fn groups_defined(&self) -> usize {
        let inner = self.0.lock().unwrap();
        let root_group_count = if inner.groups.contains_key(Root.name()) {
            1
        } else {
            0
        };
        inner.groups.len() - root_group_count
    }

    /// Returns number of groups included in the run after filtering.
    pub fn groups_included(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .groups
            .iter()
            .filter_map(|(group_name, tests)| (group_name != Root.name()).then_some(tests))
            .filter(|tests| {
                tests
                    .iter()
                    .any(|test_name| inner.included.contains(test_name))
            })
            .count()
    }

    /// Returns number of total defined tests.
    pub fn tests_defined(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner.defined.len()
    }

    /// Returns number of tests included in the run after filtering.
    pub fn tests_included(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner.included.len()
    }

    /// Returns number of tests that were launched.
    pub fn tests_launched(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::TestLaunched))
            .count()
    }

    /// Returns number of tests that passed successfully.
    pub fn tests_passed(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::TestPassed))
            .count()
    }

    /// Returns number of tests that failed.
    pub fn tests_failed(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::TestFailed))
            .count()
    }

    /// Returns number of tests that were skipped.
    pub fn tests_skipped(&self) -> usize {
        let inner = self.0.lock().unwrap();
        let direct = inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupSkipped(Task::Test)))
            .count();
        let group = inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupSkipped(Task::Group)))
            .fold(0, |acc, entry| {
                inner.iter_included(&entry.name).count() + acc
            });
        direct + group
    }

    /// Returns number of tests that were skipped because of error in some fixture.
    pub fn tests_skipped_by_fixture_err(&self) -> usize {
        let inner = self.0.lock().unwrap();
        let direct = inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupFailed(Task::Test)))
            .count();
        let group = inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupFailed(Task::Group)))
            .fold(0, |acc, entry| {
                inner.iter_included(&entry.name).count() + acc
            });
        direct + group
    }

    /// Returns number of setups that were launched.
    pub fn setups_launched(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupLaunched))
            .count()
    }

    /// Returns number of setups that passed successfully.
    pub fn setups_passed(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupPassed))
            .count()
    }

    /// Returns number of setups that were skipped.
    pub fn setups_skipped(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupSkipped(_)))
            .count()
    }

    /// Returns number of setups that failed.
    pub fn setups_failed(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupFailed(_)))
            .count()
    }

    /// Returns number of teardowns that were launched.
    pub fn teardowns_launched(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::TeardownLaunched))
            .count()
    }

    /// Returns number of teardowns that passed successfully.
    pub fn teardowns_passed(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::TeardownPassed))
            .count()
    }

    /// Returns number of teardowns that failed.
    pub fn teardowns_failed(&self) -> usize {
        let inner = self.0.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::TeardownFailed))
            .count()
    }

    /// Returns a list of names of passed tests.
    pub fn tests_passed_names(&self) -> Vec<String> {
        let inner = self.0.lock().unwrap();
        let mut names = inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::TestPassed))
            .map(|entry| entry.name.clone())
            .collect_vec();
        names.sort();
        names.dedup();
        names
    }

    /// Returns a list of names of skipped tests.
    pub fn tests_skipped_names(&self) -> Vec<String> {
        let inner = self.0.lock().unwrap();
        let mut names = inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupSkipped(Task::Test)))
            .map(|entry| &entry.name)
            .chain(
                inner
                    .events
                    .iter()
                    .filter(|entry| matches!(entry.event, Event::SetupSkipped(Task::Group)))
                    .flat_map(|entry| inner.iter_included(&entry.name)),
            )
            .cloned()
            .collect_vec();
        names.sort();
        names.dedup();
        names
    }

    /// Returns a list of names of skipped tests because of error in some fixture.
    pub fn tests_skipped_by_fixture_err_names(&self) -> Vec<String> {
        let inner = self.0.lock().unwrap();
        let mut names = inner
            .events
            .iter()
            .filter(|entry| matches!(entry.event, Event::SetupFailed(Task::Test)))
            .map(|entry| &entry.name)
            .chain(
                inner
                    .events
                    .iter()
                    .filter(|entry| matches!(entry.event, Event::SetupFailed(Task::Group)))
                    .flat_map(|entry| inner.iter_included(&entry.name)),
            )
            .cloned()
            .collect_vec();
        names.sort();
        names.dedup();
        names
    }

    /// Returns a list of names of failed tests and groups.
    pub fn failed_names(&self) -> Vec<String> {
        let inner = self.0.lock().unwrap();
        let mut names = inner
            .events
            .iter()
            .filter(|entry| entry.is_failed())
            .map(|entry| entry.name.clone())
            .collect_vec();
        names.sort();
        names.dedup();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing() {
        let stats = Statistics::new();
        stats.record(
            "foo",
            Event::TestDefined {
                group: "root".to_string(),
            },
        );
        stats.record("foo", Event::TestIncluded);
        assert!(stats.is_success());
        stats.record("foo", Event::TestFailed);
        assert!(!stats.is_success());

        let stats = Statistics::new();
        stats.record(
            "foo",
            Event::TestDefined {
                group: "root".to_string(),
            },
        );
        stats.record("foo", Event::TestIncluded);
        stats.record("foo", Event::SetupFailed(Task::Test));
        assert!(!stats.is_success());

        let stats = Statistics::new();
        stats.record(
            "foo",
            Event::TestDefined {
                group: "root".to_string(),
            },
        );
        stats.record("foo", Event::TestIncluded);
        stats.record("foo", Event::SetupFailed(Task::Group));
        assert!(!stats.is_success());

        let stats = Statistics::new();
        stats.record(
            "foo",
            Event::TestDefined {
                group: "root".to_string(),
            },
        );
        stats.record("foo", Event::TestIncluded);
        stats.record("foo", Event::TeardownFailed);
        assert!(!stats.is_success());
    }

    #[test]
    fn calculations() {
        let stats = Statistics::new();

        // Define a hierarchy of tests and groups
        stats.record(
            "root1::branch1::test1",
            Event::TestDefined {
                group: "root1::branch1".to_string(),
            },
        );
        stats.record(
            "root1::branch1::test2",
            Event::TestDefined {
                group: "root1::branch1".to_string(),
            },
        );
        stats.record(
            "root1::branch1::test3",
            Event::TestDefined {
                group: "root1::branch1".to_string(),
            },
        );
        stats.record(
            "root1::branch2::test1",
            Event::TestDefined {
                group: "root1::branch2".to_string(),
            },
        );
        stats.record(
            "root1::branch2::test2",
            Event::TestDefined {
                group: "root1::branch2".to_string(),
            },
        );
        stats.record(
            "root1::branch2::test3",
            Event::TestDefined {
                group: "root1::branch2".to_string(),
            },
        );
        stats.record(
            "root1::test1",
            Event::TestDefined {
                group: "root1".to_string(),
            },
        );
        stats.record(
            "root1::test2",
            Event::TestDefined {
                group: "root1".to_string(),
            },
        );
        stats.record(
            "root1::test3",
            Event::TestDefined {
                group: "root1".to_string(),
            },
        );
        stats.record(
            "root2::branch1::test1",
            Event::TestDefined {
                group: "root2::branch1".to_string(),
            },
        );
        stats.record(
            "root2::branch1::test2",
            Event::TestDefined {
                group: "root2::branch1".to_string(),
            },
        );
        stats.record(
            "root2::branch1::test3",
            Event::TestDefined {
                group: "root2::branch1".to_string(),
            },
        );
        stats.record(
            "root2::branch2::test1",
            Event::TestDefined {
                group: "root2::branch2".to_string(),
            },
        );
        stats.record(
            "root2::branch2::test2",
            Event::TestDefined {
                group: "root2::branch2".to_string(),
            },
        );
        stats.record(
            "root2::branch2::test3",
            Event::TestDefined {
                group: "root2::branch2".to_string(),
            },
        );
        stats.record(
            "root2::test1",
            Event::TestDefined {
                group: "root2".to_string(),
            },
        );
        stats.record(
            "root2::test2",
            Event::TestDefined {
                group: "root2".to_string(),
            },
        );
        stats.record(
            "root2::test3",
            Event::TestDefined {
                group: "root2".to_string(),
            },
        );

        // Include some of the tests in the run (simulate filtering)
        stats.record("root1::branch1::test1", Event::TestIncluded);
        stats.record("root1::branch1::test2", Event::TestIncluded);
        stats.record("root1::branch2::test3", Event::TestIncluded);
        stats.record("root1::test1", Event::TestIncluded);
        stats.record("root1::test2", Event::TestIncluded);
        stats.record("root2::branch1::test2", Event::TestIncluded);
        stats.record("root2::branch1::test3", Event::TestIncluded);
        stats.record("root2::branch2::test2", Event::TestIncluded);
        stats.record("root2::test3", Event::TestIncluded);

        // Start executing the root1 group
        stats.record("root1", Event::SetupLaunched);
        stats.record("root1", Event::SetupPassed);

        // Start executing the root1::branch1 group
        stats.record("root1::branch1", Event::SetupLaunched);
        stats.record("root1::branch1", Event::SetupPassed);

        // Start executing the root1::branch1::test1 test that will pass
        stats.record("root1::branch1::test1", Event::SetupLaunched);
        stats.record("root1::branch1::test1", Event::SetupPassed);
        stats.record("root1::branch1::test1", Event::TestLaunched);
        stats.record("root1::branch1::test1", Event::TestPassed);
        stats.record("root1::branch1::test1", Event::TeardownLaunched);
        stats.record("root1::branch1::test1", Event::TeardownPassed);

        // Start executing the root1::branch1::test2 test that will be skipped
        stats.record("root1::branch1::test2", Event::SetupLaunched);
        stats.record("root1::branch1::test2", Event::SetupSkipped(Task::Test));
        stats.record("root1::branch1::test2", Event::TeardownLaunched);
        stats.record("root1::branch1::test2", Event::TeardownPassed);

        // Finish executing the root1::branch1 group
        stats.record("root1::branch1", Event::TeardownLaunched);
        stats.record("root1::branch1", Event::TeardownPassed);

        // Start executing the root1::branch2 group that will be skipped
        stats.record("root1::branch2", Event::SetupLaunched);
        stats.record("root1::branch2", Event::SetupSkipped(Task::Group));
        stats.record("root1::branch2", Event::TeardownLaunched);
        stats.record("root1::branch2", Event::TeardownPassed);

        // Start executing the root1::test1 test that setup will fail
        stats.record("root1::test1", Event::SetupLaunched);
        stats.record("root1::test1", Event::SetupFailed(Task::Test));
        stats.record("root1::test1", Event::TeardownLaunched);
        stats.record("root1::test1", Event::TeardownPassed);

        // Start executing the root1::test2 test that will pass
        stats.record("root1::test2", Event::SetupLaunched);
        stats.record("root1::test2", Event::SetupPassed);
        stats.record("root1::test2", Event::TestLaunched);
        stats.record("root1::test2", Event::TestPassed);
        stats.record("root1::test2", Event::TeardownLaunched);
        stats.record("root1::test2", Event::TeardownPassed);

        // Finish executing the root1 group with a teardown failure
        stats.record("root1", Event::TeardownLaunched);
        stats.record("root1", Event::TeardownFailed);

        // Start executing the root2 group
        stats.record("root2", Event::SetupLaunched);
        stats.record("root2", Event::SetupPassed);

        // Start executing the root2::branch1 group that will be skipped
        stats.record("root2::branch1", Event::SetupLaunched);
        stats.record("root2::branch1", Event::SetupSkipped(Task::Group));
        stats.record("root2::branch1", Event::TeardownLaunched);
        stats.record("root2::branch1", Event::TeardownPassed);

        // Start executing the root2::branch2 group that will fail
        stats.record("root2::branch2", Event::SetupLaunched);
        stats.record("root2::branch2", Event::SetupFailed(Task::Group));
        stats.record("root2::branch2", Event::TeardownLaunched);
        stats.record("root2::branch2", Event::TeardownPassed);

        // Start executing the root2::test3 test that will fail
        stats.record("root2::test3", Event::SetupLaunched);
        stats.record("root2::test3", Event::SetupPassed);
        stats.record("root2::test3", Event::TestLaunched);
        stats.record("root2::test3", Event::TestFailed);
        stats.record("root2::test3", Event::TeardownLaunched);
        stats.record("root2::test3", Event::TeardownPassed);

        // Finish executing the root2 group
        stats.record("root2", Event::TeardownLaunched);
        stats.record("root2", Event::TeardownPassed);

        assert_eq!(stats.tests_defined(), 18);
        assert_eq!(stats.tests_included(), 9);
        assert_eq!(stats.tests_launched(), 3);
        assert_eq!(stats.tests_skipped(), 4);
        assert_eq!(stats.tests_skipped_by_fixture_err(), 2);
        assert_eq!(stats.tests_passed(), 2);
        assert_eq!(stats.tests_failed(), 1);

        assert_eq!(stats.setups_launched(), 11);
        assert_eq!(stats.setups_passed(), 6);
        assert_eq!(stats.setups_skipped(), 3);
        assert_eq!(stats.setups_failed(), 2);

        assert_eq!(stats.teardowns_launched(), 11);
        assert_eq!(stats.teardowns_passed(), 10);
        assert_eq!(stats.teardowns_failed(), 1);

        assert_eq!(
            stats.tests_passed_names(),
            vec!["root1::branch1::test1", "root1::test2"]
        );
        assert_eq!(
            stats.tests_skipped_names(),
            vec![
                "root1::branch1::test2",
                "root1::branch2::test3",
                "root2::branch1::test2",
                "root2::branch1::test3",
            ]
        );
        assert_eq!(
            stats.tests_skipped_by_fixture_err_names(),
            vec!["root1::test1", "root2::branch2::test2"]
        );
        assert_eq!(
            stats.failed_names(),
            vec!["root1", "root1::test1", "root2::branch2", "root2::test3"]
        );
    }
}
