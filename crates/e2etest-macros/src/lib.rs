/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

//! This crate provides a macros for e2etest framework

use convert_case::ccase;
use itertools::Itertools;
use proc_macro::TokenStream;
use quote::quote;
use syn::Expr;
use syn::FieldsUnnamed;
use syn::FnArg;
use syn::GenericArgument;
use syn::Ident;
use syn::Index;
use syn::ItemFn;
use syn::Path;
use syn::PathArguments;
use syn::ReturnType;
use syn::Token;
use syn::Type;
use syn::TypePath;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::parse_macro_input;
use syn::spanned::Spanned;

fn group_tests_name(name: &str) -> String {
    format!("_E2ETEST_{name}_TESTS", name = ccase!(constant, name))
}

fn group_fixture_name(name: &str) -> String {
    format!("_E2etestGroupFixture{name}", name = ccase!(pascal, name))
}

fn group_type_name(name: &str) -> String {
    format!("_E2etestGroup{name}", name = ccase!(pascal, name))
}

fn test_fixture_name(name: &str) -> String {
    format!("_E2etestTestFixture{name}", name = ccase!(pascal, name))
}

fn test_type_name(name: &str) -> String {
    format!("_E2etestTest{name}", name = ccase!(pascal, name))
}

fn test_register_name(name: &str) -> String {
    format!("_e2etest_register_{name}", name = ccase!(snake, name))
}

struct GroupParams {
    name: Ident,
    fixtures: Vec<Ident>,
}

impl Parse for GroupParams {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let _: Ident = input.parse().and_then(|v: Ident| {
            let span = v.span();
            (v == "name")
                .then_some(v)
                .ok_or(syn::Error::new(span, "expected 'name' token"))
        })?;
        let _: Token![=] = input.parse()?;
        let name: Ident = input.parse()?;
        let name = Ident::new(&name.to_string().to_lowercase(), name.span());

        let mut fixtures = Vec::new();

        while !input.is_empty() {
            let _: Token![,] = input.parse()?;

            let name: Ident = input.parse()?;
            if name == "fixtures" {
                let _: Token![=] = input.parse()?;
                let fields: FieldsUnnamed = input.parse()?;
                fixtures = fields
                    .unnamed
                    .into_iter()
                    .map(|elem| {
                        let Type::Path(type_path) = elem.ty else {
                            return Err(syn::Error::new(
                                elem.span(),
                                "Expected a type implementing Fixture",
                            ));
                        };
                        Ok(type_path.path.segments)
                    })
                    .map(|segments| {
                        segments.and_then(|segments| {
                            let span = segments.span();
                            (segments.len() == 1)
                            .then_some(segments)
                            .and_then(|segments| segments.first().cloned())
                            .ok_or(syn::Error::new(
                                span,
                                "Expected a single pathtuple of Fixture with at least one element",
                            ))
                        })
                    })
                    .map_ok(|path_segment| path_segment.ident)
                    .collect::<syn::Result<_>>()?;
            } else {
                return Err(syn::Error::new(
                    name.span(),
                    "unexpected parameter, expected 'fixtures'",
                ));
            }
        }
        Ok(Self { name, fixtures })
    }
}

/// Macro for defining a test group.
///
/// It generates a struct implementing `e2etest::Group` trait and registers it in the framework.
/// All tests in the group will be run concurrently and will share the same set of group fixtures
/// if provided.
///
/// The macro takes the following parameters:
/// - `name`: the name of the group (required)
/// - `fixtures`: a tuple of fixture types that will be set up for each test in the group
///   (optional).
#[proc_macro]
pub fn group(item: TokenStream) -> TokenStream {
    let params = parse_macro_input!(item as GroupParams);
    generate_group(params)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn generate_group(params: GroupParams) -> syn::Result<proc_macro2::TokenStream> {
    let name = params.name;
    let name_string = name.to_string();
    let fixtures = params.fixtures;
    let group_tests = Ident::new(&group_tests_name(&name_string), name.span());
    let group_fixture = Ident::new(&group_fixture_name(&name_string), name.span());
    let group_type = Ident::new(&group_type_name(&name_string), name.span());

    let expanded = quote! {
        #[e2etest::__linkme::distributed_slice]
        #[linkme(crate = e2etest::__linkme)]
        pub static #group_tests: [fn() -> Box<dyn e2etest::RunTest>];

        struct #group_fixture(#(std::sync::Arc<#fixtures>),*);
        impl e2etest::Fixture for #group_fixture {
            async fn setup(setup: &mut impl e2etest::Setup) -> Option<Self> {
                Some(Self(#(setup.setup::<#fixtures>().await?),*))
            }
            async fn teardown(self) { }
        }

        struct #group_type;

        impl e2etest::Group for #group_type {
            type Fixture = #group_fixture;

            fn name(&self) -> &str {
                concat!(module_path!(), "::", #name_string)
            }

            fn tests(&self) -> &[Box<dyn e2etest::RunTest>] {
                use std::sync::LazyLock;
                use e2etest::RunTest;

                static TESTS: LazyLock<Vec<Box<dyn e2etest::RunTest>>> = LazyLock::new(|| {
                    #group_tests.iter().map(|test_fn| test_fn()).collect()
                });
                TESTS.as_slice()
            }
        }

        #[e2etest::__linkme::distributed_slice(e2etest::E2ETEST_GROUPS)]
        #[linkme(crate = e2etest::__linkme)]
        pub fn #name() -> Box<dyn e2etest::RunGroup> {
            Box::new(#group_type)
        }
    };

    Ok(expanded)
}

struct TestParams {
    group: Option<Path>,
    timeout: Option<Expr>,
}

impl Parse for TestParams {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut group = None;
        let mut timeout = None;

        while !input.is_empty() {
            let name: Ident = input.parse()?;
            if name == "timeout" {
                let _: Token![=] = input.parse()?;
                timeout = Some(input.parse()?);
            } else if name == "group" {
                let _: Token![=] = input.parse()?;
                group = Some(input.parse()?);
            } else {
                return Err(syn::Error::new(
                    name.span(),
                    "unexpected parameter, expected 'group' or 'timeout'",
                ));
            }

            if input.is_empty() {
                break;
            }
            let _: Token![,] = input.parse()?;
        }

        Ok(Self { group, timeout })
    }
}

fn take_fixtures(run: &ItemFn) -> syn::Result<Vec<TypePath>> {
    let fixtures: Vec<_> = run
        .sig
        .inputs
        .iter()
        .map(|arg| {
            if let FnArg::Typed(pat_type) = arg
                && let Type::Path(type_path) = &*pat_type.ty
                && let Some(path_segment) = type_path.path.segments.first()
                && path_segment.ident == "Arc"
                && let PathArguments::AngleBracketed(angle_bracketed) = &path_segment.arguments
                && let Some(generic_arg) = angle_bracketed.args.first()
                && let GenericArgument::Type(Type::Path(fixture_type)) = generic_arg
            {
                Ok(fixture_type.clone())
            } else {
                Err(syn::Error::new(
                    arg.span(),
                    "Expected arguments of type Arc<Fixture>",
                ))
            }
        })
        .collect::<syn::Result<_>>()?;
    if fixtures.is_empty() {
        Err(syn::Error::new(
            run.sig.inputs.span(),
            "Expected the test function to have at least one argument of type Arc<Fixture>",
        ))
    } else {
        Ok(fixtures)
    }
}

/// Macro for defining a test.
///
/// It generates a struct implementing `e2etest::Test` trait and registers it in the framework.
/// If the test belongs to a group, it will be registered in the group and will share the same set
/// of group fixtures and will be run concurrently with other tests in the group. Otherwise, it
/// will be registered as a standalone test and will be run sequentially with other standalone
/// tests.
///
/// The macro takes the following parameters:
/// - `group`: the path to the group this test belongs to (optional)
/// - `timeout`: an expression resolved to `Duration` as the timeout for the test (optional)
///
/// The test function must be async, return `()`, and take as arguments a list of `Arc<Fixture>`
/// as a list of fixtures used inside the test.
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let params = parse_macro_input!(attr as TestParams);
    let run = parse_macro_input!(item as ItemFn);
    generate_test(params, run)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn generate_test(params: TestParams, run: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    let register_test = if let Some(group) = params.group {
        let Some(last) = group.segments.last() else {
            return Err(syn::Error::new(
                group.segments.span(),
                "Expected group path to have at least one segment",
            ));
        };
        let group_name = last.ident.to_string();
        let mut group_tests = group.clone();
        let Some(last) = group_tests.segments.last_mut() else {
            return Err(syn::Error::new(
                group_tests.segments.span(),
                "Expected group path to have at least one segment",
            ));
        };
        last.ident = Ident::new(&group_tests_name(&group_name), last.ident.span());
        quote! {
            #[e2etest::__linkme::distributed_slice(#group_tests)]
            #[linkme(crate = e2etest::__linkme)]
        }
    } else {
        quote! {
            #[e2etest::__linkme::distributed_slice(e2etest::E2ETEST_TESTS)]
            #[linkme(crate = e2etest::__linkme)]
        }
    };

    let name = run.sig.ident.clone();
    let name_string = name.to_string();
    let test_fixture = Ident::new(&test_fixture_name(&name_string), name.span());
    let test_type = Ident::new(&test_type_name(&name_string), name.span());
    let test_register = Ident::new(&test_register_name(&name_string), name.span());
    if run.sig.asyncness.is_none() {
        return Err(syn::Error::new(
            run.sig.span(),
            "Expected the test function to be async",
        ));
    }
    if !matches!(run.sig.output, ReturnType::Default) {
        return Err(syn::Error::new(
            run.sig.output.span(),
            "Expected the test function to return ()",
        ));
    }
    let run_attrs = &run.attrs;
    let run_vis = &run.vis;
    let run_sig = &run.sig;
    let run_block = &run.block;

    let timeout = if let Some(timeout) = &params.timeout {
        quote! {
            fn timeout(&self) -> Option<std::time::Duration> {
                Some(#timeout)
            }
        }
    } else {
        quote! {}
    };

    let fixtures = take_fixtures(&run)?;
    let fixtures_range = (0..fixtures.len()).map(Index::from);

    let expanded = quote! {
        struct #test_fixture(#(std::sync::Arc<#fixtures>),*);
        impl e2etest::Fixture for #test_fixture {
            async fn setup(setup: &mut impl e2etest::Setup) -> Option<Self> {
                Some(Self(#(setup.setup::<#fixtures>().await?),*))
            }
            async fn teardown(self) { }
        }

        struct #test_type;

        impl e2etest::Test for #test_type {
            type Fixture = #test_fixture;

            fn name(&self) -> &str {
                concat!(module_path!(), "::", #name_string)
            }

            #timeout

            fn run(&self, fixture: std::sync::Arc<#test_fixture>) -> impl std::future::Future<Output = ()> + Send + 'static {
                async move {
                    #name(#(std::sync::Arc::clone(&fixture.#fixtures_range)),*).await;
                }
            }
        }

        #register_test
        fn #test_register() -> Box<dyn e2etest::RunTest> {
            Box::new(#test_type)
        }

        #(#run_attrs)* #run_vis #run_sig {
            e2etest::__async_backtrace::frame!(async move #run_block).await
        }
    };

    Ok(expanded)
}
