/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use axum::Router;
use axum::extract::Path;
use axum::routing::get;
use e2etest::Config;
use e2etest::ConfigUnshare;
use reqwest::Client;
use std::env;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time;
use tracing::error;
use tracing::info;

struct Permanent;

struct Fixture {
    client: Client,
    url: String,
}

const MOCK_OPERATION_DURATION: Duration = Duration::from_secs(1);

impl e2etest::Fixture for Fixture {
    async fn setup(_setup: &mut impl e2etest::Setup) -> Option<Self> {
        info!("Creating http router");
        let app = Router::new().route(
            "/add-one/{num}",
            get(|Path(num): Path<usize>| async move {
                time::sleep(MOCK_OPERATION_DURATION).await;
                format!("sum: {}", num + 1)
            }),
        );
        info!("Starting tcp listener");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .inspect_err(|err| error!("Failed to bind to address: {err}"))
            .ok()?;
        let addr = listener
            .local_addr()
            .inspect_err(|err| error!("Failed to get local address: {err}"))
            .ok()?;
        info!("Server starting at http://{addr}");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .inspect_err(|err| error!("Failed to run server: {err}"))
                .expect("Failed to run server");
        });
        let client = Client::new();
        let url = format!("http://{}", addr);
        Some(Fixture { client, url })
    }

    async fn teardown(self) {}
}

#[e2etest::test]
async fn exclusive1(fixture: Arc<Fixture>) {
    let result = fixture
        .client
        .get(format!("{url}/add-one/1", url = fixture.url))
        .send()
        .await
        .expect("Failed to send request")
        .text()
        .await
        .expect("Failed to read response");
    info!("result: {result}");
    assert_eq!(result, "sum: 2");
}

#[e2etest::test]
async fn exclusive2(fixture: Arc<Fixture>) {
    let result = fixture
        .client
        .get(format!("{url}/add-one/2", url = fixture.url))
        .send()
        .await
        .expect("Failed to send request")
        .text()
        .await
        .expect("Failed to read response");
    info!("result: {result}");
    assert_eq!(result, "sum: 3");
}

e2etest::group!(name = shared, fixtures = (Fixture));

#[e2etest::test(group = shared)]
async fn shared1(fixture: Arc<Fixture>) {
    let result = fixture
        .client
        .get(format!("{url}/add-one/3", url = fixture.url))
        .send()
        .await
        .expect("Failed to send request")
        .text()
        .await
        .expect("Failed to read response");
    info!("result: {result}");
    assert_eq!(result, "sum: 4");
}

#[e2etest::test(group = shared)]
async fn shared2(fixture: Arc<Fixture>) {
    let result = fixture
        .client
        .get(format!("{url}/add-one/4", url = fixture.url))
        .send()
        .await
        .expect("Failed to send request")
        .text()
        .await
        .expect("Failed to read response");
    info!("result: {result}");
    assert_eq!(result, "sum: 5");
}

fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let filters = env::args().skip(1);
    if let Some(unshare_info) = e2etest::unshare_info() {
        let config = ConfigUnshare::default()
            .with_permanent_fixture(Permanent)
            .with_concurrency(100);
        e2etest::run_in_unshare(config, unshare_info);
        return ExitCode::SUCCESS;
    };

    info!("group names: {:?}", e2etest::group_names());

    info!("test names: {:?}", e2etest::test_names());

    let config = Config::default().with_concurrency(100);
    let config = filters.fold(config, |config, filter| config.with_filter(filter));

    let stats = e2etest::run(config);
    if stats.is_success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
