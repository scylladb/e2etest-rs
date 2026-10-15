/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use crate::RootGroup;
use std::collections::BTreeSet;
use std::sync::Arc;
use wildmatch::WildMatch;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Restricted {
    None,
    Group(String),
    Test(String),
}

/// Represents the filter configuration for test execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Filter {
    All,
    Some {
        tests: Arc<BTreeSet<String>>,
        groups: Arc<BTreeSet<String>>,
    },
}

impl Filter {
    pub(crate) fn empty() -> Self {
        Self::All
    }

    /// Parse command line filters into the expected filter format for test execution.
    pub(crate) fn new(restricted: &Restricted, filters: &[String], group: &impl RootGroup) -> Self {
        let filters = if filters.is_empty() {
            if matches!(restricted, Restricted::None) {
                return Self::All;
            }
            &["*".to_string()]
        } else {
            filters
        };

        let mut filtered_tests = BTreeSet::new();
        let mut filtered_groups = BTreeSet::new();

        filters
            .iter()
            .map(|filter| WildMatch::new(filter))
            .for_each(|filter| {
                group
                    .group_test_names()
                    .filter(|(group_name, test_name)| match &restricted {
                        Restricted::None => true,
                        Restricted::Group(restricted_group) => group_name == restricted_group,
                        Restricted::Test(restricted_test) => test_name == restricted_test,
                    })
                    .filter(|(_, test_name)| filter.matches(test_name))
                    .for_each(|(group_name, test_name)| {
                        filtered_groups.insert(group_name.to_string());
                        filtered_tests.insert(test_name.to_string());
                    });
            });

        Self::Some {
            tests: Arc::new(filtered_tests),
            groups: Arc::new(filtered_groups),
        }
    }

    pub(crate) fn consider_group(&self, group_name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Some { groups, .. } => groups.contains(group_name),
        }
    }

    pub(crate) fn consider_test(&self, test_name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Some { tests, .. } => tests.contains(test_name),
        }
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
                    &[
                        "full_scan::scan_index",
                        "full_scan::scan_undex",
                        "full_scan::scan_all",
                    ],
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
        let result = Filter::new(&Restricted::None, &filters, &test_cases);
        assert_eq!(result, Filter::All);
    }

    #[test]
    fn test_empty_filters_runs_all() {
        let test_cases = make_test_cases();
        let filters: Vec<String> = vec!["*".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        // It should contain all available test cases

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(
                    ["crud", "full_scan", "other"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
                tests: Arc::new(
                    [
                        "crud::drop_index",
                        "crud::simple_create",
                        "full_scan::scan_all",
                        "full_scan::scan_index",
                        "full_scan::scan_undex",
                        "other::misc",
                        "other::simple_misc",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect()
                ),
            }
        );
    }

    #[test]
    fn test_wrong_filters_runs_nothing() {
        let test_cases = make_test_cases();
        let filters: Vec<String> = vec!["bad_name".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(BTreeSet::new()),
                tests: Arc::new(BTreeSet::new()),
            }
        );
    }

    #[test]
    fn test_module_partial_match() {
        let test_cases = make_test_cases();
        let filters = vec!["*crud*".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["crud"].into_iter().map(String::from).collect()),
                tests: Arc::new(
                    ["crud::drop_index", "crud::simple_create"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }

    #[test]
    fn test_test_case_partial_match() {
        let test_cases = make_test_cases();
        let filters = vec!["*simple*".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["crud", "other"].into_iter().map(String::from).collect()),
                tests: Arc::new(
                    ["crud::simple_create", "other::simple_misc"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }

    #[test]
    fn test_test_case_question_sign() {
        let test_cases = make_test_cases();
        let filters = vec!["*_?ndex*".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(
                    ["crud", "full_scan"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
                tests: Arc::new(
                    [
                        "crud::drop_index",
                        "full_scan::scan_index",
                        "full_scan::scan_undex",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect()
                ),
            }
        );
    }

    #[test]
    fn test_test_case_restriction_group() {
        let test_cases = make_test_cases();
        let filters = vec!["*_?ndex*".into()];
        let result = Filter::new(
            &Restricted::Group("crud".to_string()),
            &filters,
            &test_cases,
        );

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["crud"].into_iter().map(String::from).collect()),
                tests: Arc::new(["crud::drop_index"].into_iter().map(String::from).collect()),
            }
        );
    }

    #[test]
    fn test_test_case_restriction_test() {
        let test_cases = make_test_cases();
        let filters = vec!["*_?ndex*".into()];
        let result = Filter::new(
            &Restricted::Test("full_scan::scan_undex".to_string()),
            &filters,
            &test_cases,
        );

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["full_scan"].into_iter().map(String::from).collect()),
                tests: Arc::new(
                    ["full_scan::scan_undex"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }

    #[test]
    fn test_restriction_group_empty_filters() {
        let test_cases = make_test_cases();
        let filters = vec![];
        let result = Filter::new(
            &Restricted::Group("crud".to_string()),
            &filters,
            &test_cases,
        );

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["crud"].into_iter().map(String::from).collect()),
                tests: Arc::new(
                    ["crud::drop_index", "crud::simple_create"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }

    #[test]
    fn test_module_and_test_case_syntax() {
        let test_cases = make_test_cases();
        let filters = vec!["*crud*::*simple*".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["crud"].into_iter().map(String::from).collect()),
                tests: Arc::new(
                    ["crud::simple_create"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }

    #[test]
    fn test_module_and_empty_test_case_syntax() {
        let test_cases = make_test_cases();
        let filters = vec!["*crud*::*".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["crud"].into_iter().map(String::from).collect()),
                tests: Arc::new(
                    ["crud::drop_index", "crud::simple_create"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }

    #[test]
    fn test_empty_module_and_test_case_syntax() {
        let test_cases = make_test_cases();
        let filters = vec!["*::*simple*".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["crud", "other"].into_iter().map(String::from).collect()),
                tests: Arc::new(
                    ["crud::simple_create", "other::simple_misc"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }

    #[test]
    fn test_exact_module_match_syntax() {
        let test_cases = make_overlapping_test_cases();
        let filters = vec!["crud::*".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["crud"].into_iter().map(String::from).collect()),
                tests: Arc::new(
                    ["crud::simple_create", "crud::simple_create_extra"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }

    #[test]
    fn test_exact_test_case_match_syntax() {
        let test_cases = make_overlapping_test_cases();
        let filters = vec!["*::simple_create".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(
                    ["crud", "crud_extra"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
                tests: Arc::new(
                    ["crud::simple_create", "crud_extra::simple_create"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }

    #[test]
    fn test_exact_module_and_test_case_syntax() {
        let test_cases = make_overlapping_test_cases();
        let filters = vec!["crud::simple_create".into()];
        let result = Filter::new(&Restricted::None, &filters, &test_cases);

        assert_eq!(
            result,
            Filter::Some {
                groups: Arc::new(["crud"].into_iter().map(String::from).collect()),
                tests: Arc::new(
                    ["crud::simple_create"]
                        .into_iter()
                        .map(String::from)
                        .collect()
                ),
            }
        );
    }
}
