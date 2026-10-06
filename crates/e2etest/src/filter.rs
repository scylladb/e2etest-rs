/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use crate::RootGroup;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FilterMatcher<'a> {
    Any,
    Partial(&'a str),
    Exact(&'a str),
    GroupPartial(&'a str),
    GroupExact(&'a str),
    TestPartial(&'a str),
    TestExact(&'a str),
}

impl<'a> FilterMatcher<'a> {
    fn new(filter: &'a str) -> Self {
        if filter.is_empty() {
            Self::Any
        } else if let Some(filter) = filter.strip_prefix("::") {
            if let Some(filter) = filter
                .strip_prefix('"')
                .and_then(|filter| filter.strip_suffix('"'))
            {
                Self::TestExact(filter)
            } else {
                Self::TestPartial(filter)
            }
        } else if let Some(filter) = filter.strip_suffix("::") {
            if let Some(filter) = filter
                .strip_prefix('"')
                .and_then(|filter| filter.strip_suffix('"'))
            {
                Self::GroupExact(filter)
            } else {
                Self::GroupPartial(filter)
            }
        } else if let Some(filter) = filter
            .strip_prefix('"')
            .and_then(|filter| filter.strip_suffix('"'))
        {
            Self::Exact(filter)
        } else {
            Self::Partial(filter)
        }
    }

    fn matches_group(self, candidate: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Partial(filter) => candidate.contains(filter),
            Self::Exact(filter) => candidate == filter,
            Self::GroupPartial(filter) => candidate.contains(filter),
            Self::GroupExact(filter) => candidate == filter,
            _ => false,
        }
    }

    fn matches_test(self, candidate: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Partial(filter) => candidate.contains(filter),
            Self::Exact(filter) => candidate == filter,
            Self::TestPartial(filter) => candidate.contains(filter),
            Self::TestExact(filter) => candidate == filter,
            _ => false,
        }
    }
}

/// Represents the filter configuration for test execution.
#[derive(Clone, Debug)]
pub(crate) struct Filter {
    /// - Key: test group name (e.g., "crud", "full_scan")
    /// - Value: HashSet of specific test names within that group (empty means run all tests in
    ///   group)
    tests: Arc<HashMap<String, HashSet<String>>>,
}

impl Filter {
    pub(crate) fn empty() -> Self {
        Self {
            tests: Arc::new(HashMap::new()),
        }
    }

    /// Parse command line filters into the expected filter format for test execution.
    pub(crate) fn new<'a>(
        filters: impl IntoIterator<Item = &'a (impl AsRef<str> + 'a)>,
        group: &impl RootGroup,
    ) -> Self {
        let mut filter_map = HashMap::new();
        let mut group_set = HashSet::new();

        filters
            .into_iter()
            .map(AsRef::as_ref)
            .map(FilterMatcher::new)
            .for_each(|filter| {
                group
                    .group_test_names()
                    .map(|(group_name, test_name)| {
                        (
                            group_name,
                            test_name,
                            filter.matches_group(group_name),
                            filter.matches_test(test_name),
                        )
                    })
                    .filter(|(_, _, match_group, match_test)| *match_group || *match_test)
                    .for_each(|(group_name, test_name, match_group, _)| {
                        if match_group {
                            group_set.insert(group_name.to_string());
                            filter_map.insert(group_name.to_string(), HashSet::new());
                            return;
                        }
                        match filter_map.entry(group_name.to_string()) {
                            Entry::Occupied(mut entry) => {
                                if entry.get().is_empty() {
                                    // If the group is already set to run all tests,
                                    // we don't need to add specific tests
                                    return;
                                }
                                entry.get_mut().insert(test_name.to_string());
                            }
                            Entry::Vacant(entry) => {
                                entry.insert([test_name.to_string()].into_iter().collect());
                            }
                        };
                    });
            });

        Self {
            tests: Arc::new(filter_map),
        }
    }

    pub(crate) fn consider_group(&self, group_name: &str) -> bool {
        self.tests.is_empty() || self.tests.contains_key(group_name)
    }

    pub(crate) fn consider_test(&self, group_name: &str, test_name: &str) -> bool {
        self.tests.is_empty()
            || self
                .tests
                .get(group_name)
                .is_some_and(|tests| tests.is_empty() || tests.contains(test_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RunGroup;
    use crate::fixture::Fixture;
    use crate::fixture::Setup;
    use crate::group::Group;
    use crate::test::RunTest;
    use crate::test::Test;

    #[derive(Clone)]
    struct GroupFixture;

    impl From<&GroupFixture> for GroupFixture {
        fn from(_: &GroupFixture) -> Self {
            Self
        }
    }

    impl Fixture for GroupFixture {
        async fn setup(_: &mut impl Setup) -> Option<Self> {
            Some(Self)
        }
        async fn teardown(self) {}
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

        #[allow(clippy::manual_async_fn)]
        fn run(&self, _: Arc<Self::Fixture>) -> impl Future<Output = ()> + Send + 'static {
            async {}
        }
    }

    struct GroupImpl {
        name: String,
        tests: Vec<Box<dyn RunTest>>,
        groups: Vec<Box<dyn RunGroup>>,
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
            &self.groups
        }
    }

    fn make_dummy_group(group_name: &str, test_names: &[&str]) -> Box<dyn RunGroup> {
        Box::new(GroupImpl {
            name: group_name.to_string(),
            tests: test_names
                .iter()
                .map(|&name| Box::new(TestImpl(name.to_string())) as Box<dyn RunTest>)
                .collect(),
            groups: vec![],
        })
    }

    fn make_test_cases() -> impl RootGroup {
        GroupImpl {
            name: "root".to_string(),
            tests: vec![],
            groups: vec![
                make_dummy_group("crud", &["crud::simple_create", "crud::drop_index"]),
                make_dummy_group(
                    "full_scan",
                    &["full_scan::scan_index", "full_scan::scan_all"],
                ),
                make_dummy_group("other", &["other::misc", "other::simple_misc"]),
            ],
        }
    }

    fn make_overlapping_test_cases() -> impl RootGroup {
        GroupImpl {
            name: "root".to_string(),
            tests: vec![],
            groups: vec![
                make_dummy_group(
                    "crud",
                    &["crud::simple_create", "crud::simple_create_extra"],
                ),
                make_dummy_group(
                    "crud_extra",
                    &[
                        "crud_extra::simple_create",
                        "crud_extra::simple_create_additional",
                    ],
                ),
            ],
        }
    }

    #[test]
    fn test_no_filters_runs_all() {
        let test_cases = make_test_cases();
        let filters: Vec<String> = vec![];
        let result = Filter::new(&filters, &test_cases);
        assert!(result.tests.is_empty());
    }

    #[test]
    fn test_empty_filters_runs_all() {
        let test_cases = make_test_cases();
        let filters: Vec<String> = vec!["::".to_string()];
        let result = Filter::new(&filters, &test_cases);
        // It should contain all available test groups with all test cases (running all)
        assert_eq!(result.tests.len(), 3);
        assert_eq!(result.tests["crud"].len(), 2);
        assert_eq!(result.tests["full_scan"].len(), 2);
        assert_eq!(result.tests["other"].len(), 2);
    }

    #[test]
    fn test_group_partial_match() {
        let test_cases = make_test_cases();
        let filters = vec!["crud".to_string()];
        let result = Filter::new(&filters, &test_cases);
        assert!(result.tests.contains_key("crud"));
        assert!(result.tests["crud"].is_empty());
        assert_eq!(result.tests.len(), 1);
    }

    #[test]
    fn test_test_case_partial_match() {
        let test_cases = make_test_cases();
        let filters = vec!["simple".to_string()];
        let result = Filter::new(&filters, &test_cases);
        assert!(result.tests["crud"].contains("crud::simple_create"));
        assert!(result.tests["other"].contains("other::simple_misc"));
        assert_eq!(result.tests.len(), 2);
    }

    #[test]
    fn test_group_and_test_case_syntax() {
        let test_cases = make_test_cases();
        let filters = vec!["crud::simple".to_string()];
        let result = Filter::new(&filters, &test_cases);
        assert!(result.tests["crud"].contains("crud::simple_create"));
        assert_eq!(result.tests.len(), 1);
    }

    #[test]
    fn test_group_and_empty_test_case_syntax() {
        let test_cases = make_test_cases();
        let filters = vec!["crud::".to_string()];
        let result = Filter::new(&filters, &test_cases);
        assert!(result.tests.contains_key("crud"));
        assert!(result.tests["crud"].is_empty());
        assert_eq!(result.tests.len(), 1);
    }

    #[test]
    fn test_empty_group_and_test_case_syntax() {
        let test_cases = make_test_cases();
        let filters = vec!["::simple".to_string()];
        let result = Filter::new(&filters, &test_cases);
        assert!(result.tests["crud"].contains("crud::simple_create"));
        assert!(result.tests["other"].contains("other::simple_misc"));
        assert_eq!(result.tests.len(), 2);
    }

    #[test]
    fn test_exact_group_match_syntax() {
        let test_cases = make_overlapping_test_cases();
        let filters = vec!["\"crud\"::".to_string()];
        let result = Filter::new(&filters, &test_cases);
        assert!(result.tests.contains_key("crud"));
        assert!(result.tests["crud"].is_empty());
        assert_eq!(result.tests.len(), 1);
    }

    #[test]
    fn test_exact_test_case_match_syntax() {
        let test_cases = make_overlapping_test_cases();
        let filters = vec!["::\"crud::simple_create\"".to_string()];
        let result = Filter::new(&filters, &test_cases);
        assert!(result.tests["crud"].contains("crud::simple_create"));
        assert!(!result.tests["crud"].contains("crud::simple_create_extra"));
        assert_eq!(result.tests.len(), 1);
    }

    #[test]
    fn test_exact_group_and_test_case_syntax() {
        let test_cases = make_overlapping_test_cases();
        let filters = vec!["\"crud::simple_create\"".to_string()];
        let result = Filter::new(&filters, &test_cases);
        assert!(result.tests["crud"].contains("crud::simple_create"));
        assert!(!result.tests["crud"].contains("crud::simple_create_extra"));
        assert_eq!(result.tests.len(), 1);
    }
}
