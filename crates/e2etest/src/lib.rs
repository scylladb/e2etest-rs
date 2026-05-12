/*
 * Copyright 2025-present ScyllaDB
 * SPDX-License-Identifier: LicenseRef-ScyllaDB-Source-Available-1.0
 */

mod testcase;

use async_backtrace::frame;
use async_backtrace::framed;
use clap::Parser;
use clap::Subcommand;
use std::collections::HashMap;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::panic;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
pub use testcase::TestCase;
pub use testcase::xfail;
use tokio::fs;
use tokio::runtime::Builder;
use tokio::runtime::Handle;
use tokio::task;
use tokio::time;
use tracing::error;
use tracing::info;
use tracing::warn;

#[derive(Parser)]
#[clap(version)]
struct Args<T: clap::Args> {
    #[command(subcommand)]
    command: Command<T>,
}

#[derive(Subcommand)]
enum Command<T: clap::Args> {
    /// Print the list of available tests and exit.
    List,

    /// Run the vector-search-validator tests.
    Run {
        #[clap(flatten)]
        inner: T,

        /// Filters to select specific tests to run.
        /// The syntax is as follows:
        ///     `<partially_matching_test_file_name>::<partially_matching_test_case_name>`
        /// Wrap either side in double quotes to require an exact match, for example:
        ///     `"crud"::`
        ///     `::"simple_create"`
        /// Without specifying `::`, the filter will try to match both the file and test names.
        #[arg(value_name = "FILTER")]
        filters: Vec<String>,
    },
}

#[framed]
pub async fn file_exists(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path).await else {
        return false;
    };
    metadata.is_file()
}

#[framed]
pub async fn executable_exists(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path).await else {
        return false;
    };
    metadata.is_file() && (metadata.permissions().mode() & 0o111 != 0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FilterMatcher<'a> {
    Any,
    Partial(&'a str),
    Exact(&'a str),
}

impl<'a> FilterMatcher<'a> {
    fn new(filter: &'a str) -> Self {
        if filter.is_empty() {
            Self::Any
        } else if let Some(filter) = filter
            .strip_prefix('"')
            .and_then(|filter| filter.strip_suffix('"'))
        {
            Self::Exact(filter)
        } else {
            Self::Partial(filter)
        }
    }

    fn matches(self, candidate: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Partial(filter) => candidate.contains(filter),
            Self::Exact(filter) => candidate == filter,
        }
    }
}

fn fetch_matching_tests<F>(filter: FilterMatcher<'_>, test_case: &TestCase<F>) -> HashSet<String>
where
    F: Clone + Send + Sync + 'static,
{
    test_case
        .tests()
        .iter()
        .filter_map(|(test_name, _, _)| {
            if filter.matches(test_name) {
                Some(test_name.clone())
            } else {
                None
            }
        })
        .collect()
}

fn update_filter_map(
    filter_map: &mut HashMap<String, HashSet<String>>,
    file_name: &str,
    matching_tests: HashSet<String>,
) {
    // If this file already has some tests selected, merge them
    filter_map
        .entry(file_name.to_string())
        .and_modify(|existing| {
            if !existing.is_empty() {
                existing.extend(matching_tests.iter().cloned());
            }
        })
        .or_insert(matching_tests);
}

/// Parse command line filters into the expected filter format for test execution.
/// Returns a HashMap where:
/// - Key: test file name (e.g., "crud", "full_scan")
/// - Value: HashSet of specific test names within that file (empty means run all tests in file)
fn parse_test_filters<F>(
    filters: &[String],
    test_cases: &[(String, TestCase<F>)],
) -> HashMap<String, HashSet<String>>
where
    F: Clone + Send + Sync + 'static,
{
    if filters.is_empty() {
        return HashMap::new(); // Run all tests
    }

    let mut filter_map: HashMap<String, HashSet<String>> = HashMap::new();

    for filter in filters {
        // Check for <file>::<test> syntax
        if let Some((file_part, test_part)) = filter.split_once("::") {
            let file_filter = FilterMatcher::new(file_part);
            let test_filter = FilterMatcher::new(test_part);

            for (file_name, test_case) in test_cases {
                if !file_filter.matches(file_name) {
                    continue;
                }

                if matches!(test_filter, FilterMatcher::Any) {
                    filter_map.entry(file_name.to_string()).or_default();
                    continue;
                }

                let matching_tests = fetch_matching_tests(test_filter, test_case);
                if !matching_tests.is_empty() {
                    update_filter_map(&mut filter_map, file_name, matching_tests);
                }
            }
        } else {
            // Not found `::`, check for matching both file and test case name
            let filter = FilterMatcher::new(filter);

            for (file_name, test_case) in test_cases {
                if filter.matches(file_name) {
                    filter_map.entry(file_name.to_string()).or_default();
                }
                let matching_tests = fetch_matching_tests(filter, test_case);
                if !matching_tests.is_empty() {
                    update_filter_map(&mut filter_map, file_name, matching_tests);
                }
            }
        }
    }

    filter_map
}

#[framed]
pub fn run<A, F>(
    init: impl FnOnce(&A),
    register: impl AsyncFnOnce() -> Vec<(String, TestCase<F>)>,
    fixture: impl AsyncFnOnce(&A) -> F,
) -> Result<(), &'static str>
where
    A: clap::Args,
    F: Clone + Send + Sync + 'static,
{
    let args = Args::parse();

    if let Command::Run { inner, .. } = &args.command {
        init(inner);
    }
    panic::set_hook(Box::new(|info| {
        error!("{info}");
    }));

    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(frame!(async move {
            let test_cases = register().await;
            let (inner, filters) = match &args.command {
                Command::Run { inner, filters } => (inner, filters),
                Command::List => {
                    test_cases
                        .into_iter()
                        .flat_map(|(test_case_name, test_case)| {
                            let tests: Vec<_> = test_case
                                .tests()
                                .iter()
                                .map(move |(test_name, _, _)| test_name.clone())
                                .collect();
                            tests
                                .into_iter()
                                .map(move |test_name| (test_case_name.clone(), test_name))
                        })
                        .for_each(|(test_case_name, test_name)| {
                            println!("{test_case_name}::{test_name}");
                        });
                    return Ok(());
                }
            };

            let filter_map = parse_test_filters(filters, &test_cases);

            let report =
                testcase::run(fixture(inner).await, test_cases, Arc::new(filter_map)).await;

            info!("Waiting for all tasks to finish...");
            const FINISH_TASKS_TIMEOUT: Duration = Duration::from_secs(10);
            if time::timeout(FINISH_TASKS_TIMEOUT, async {
                while Handle::current().metrics().num_alive_tasks() > 0 {
                    task::yield_now().await;
                }
            })
            .await
            .is_err()
            {
                error!("Timed out waiting for tasks to finish");
            } else {
                info!("All tasks finished");
            }

            if let Some(xpassed_tests) = report.xpassed_tests_summary() {
                warn!("{xpassed_tests}");
            }

            if let Some(failed_tests) = report.failed_tests_summary() {
                error!("{failed_tests}");
            }

            report
                .is_success()
                .then_some(())
                .ok_or("Some vector-search-validator tests failed")
        }))
}

#[cfg(test)]
pub(crate) mod validator_tests {
    use super::*;

    fn make_dummy_test_cases(test_names: &[&str]) -> TestCase<()> {
        let mut tc = TestCase::empty();
        for &name in test_names {
            tc = tc.with_test(
                name.to_string(),
                std::time::Duration::ZERO,
                |_actors| async {},
            );
        }
        tc
    }

    fn make_test_cases() -> Vec<(String, TestCase<()>)> {
        vec![
            (
                "crud".to_string(),
                make_dummy_test_cases(&["simple_create", "drop_index"]),
            ),
            (
                "full_scan".to_string(),
                make_dummy_test_cases(&["scan_index", "scan_all"]),
            ),
            (
                "other".to_string(),
                make_dummy_test_cases(&["misc", "simple_misc"]),
            ),
        ]
    }

    fn make_overlapping_test_cases() -> Vec<(String, TestCase<()>)> {
        vec![
            (
                "crud".to_string(),
                make_dummy_test_cases(&["simple_create", "simple_create_extra"]),
            ),
            (
                "crud_extra".to_string(),
                make_dummy_test_cases(&["simple_create", "simple_create_additional"]),
            ),
        ]
    }

    #[test]
    fn test_no_filters_runs_all() {
        let test_cases = make_test_cases();
        let filters: Vec<String> = vec![];
        let result = parse_test_filters(&filters, &test_cases);
        assert!(result.is_empty());
    }

    #[test]
    fn test_empty_filters_runs_all() {
        let test_cases = make_test_cases();
        let filters: Vec<String> = vec!["::".to_string()];
        let result = parse_test_filters(&filters, &test_cases);
        // It should contain all available test files with empty test cases (running all)
        assert_eq!(result.len(), 3);
        assert!(result["crud"].is_empty());
        assert!(result["full_scan"].is_empty());
        assert!(result["other"].is_empty());
    }

    #[test]
    fn test_file_partial_match() {
        let test_cases = make_test_cases();
        let filters = vec!["crud".to_string()];
        let result = parse_test_filters(&filters, &test_cases);
        assert!(result.contains_key("crud"));
        assert!(result["crud"].is_empty());
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_test_case_partial_match() {
        let test_cases = make_test_cases();
        let filters = vec!["simple".to_string()];
        let result = parse_test_filters(&filters, &test_cases);
        assert!(result["crud"].contains("simple_create"));
        assert!(result["other"].contains("simple_misc"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_file_and_test_case_syntax() {
        let test_cases = make_test_cases();
        let filters = vec!["crud::simple".to_string()];
        let result = parse_test_filters(&filters, &test_cases);
        assert!(result["crud"].contains("simple_create"));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_file_and_empty_test_case_syntax() {
        let test_cases = make_test_cases();
        let filters = vec!["crud::".to_string()];
        let result = parse_test_filters(&filters, &test_cases);
        assert!(result.contains_key("crud"));
        assert!(result["crud"].is_empty());
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_empty_file_and_test_case_syntax() {
        let test_cases = make_test_cases();
        let filters = vec!["::simple".to_string()];
        let result = parse_test_filters(&filters, &test_cases);
        assert!(result["crud"].contains("simple_create"));
        assert!(result["other"].contains("simple_misc"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_exact_file_match_syntax() {
        let test_cases = make_overlapping_test_cases();
        let filters = vec!["\"crud\"::".to_string()];
        let result = parse_test_filters(&filters, &test_cases);
        assert!(result.contains_key("crud"));
        assert!(result["crud"].is_empty());
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_exact_test_case_match_syntax() {
        let test_cases = make_overlapping_test_cases();
        let filters = vec!["::\"simple_create\"".to_string()];
        let result = parse_test_filters(&filters, &test_cases);
        assert!(result["crud"].contains("simple_create"));
        assert!(!result["crud"].contains("simple_create_extra"));
        assert!(result["crud_extra"].contains("simple_create"));
        assert!(!result["crud_extra"].contains("simple_create_additional"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_exact_file_and_test_case_syntax() {
        let test_cases = make_overlapping_test_cases();
        let filters = vec!["\"crud\"::\"simple_create\"".to_string()];
        let result = parse_test_filters(&filters, &test_cases);
        assert!(result["crud"].contains("simple_create"));
        assert!(!result["crud"].contains("simple_create_extra"));
        assert_eq!(result.len(), 1);
    }
}
