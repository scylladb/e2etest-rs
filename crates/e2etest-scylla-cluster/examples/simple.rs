/*
 * Copyright 2026-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

use e2etest::Config;
use e2etest::ConfigInUnshare;
use e2etest_dns::Dns;
use e2etest_scylla_cluster::ScyllaCluster;
use e2etest_scylla_cluster::ScyllaClusterExt;
use e2etest_scylla_cluster::ScyllaNodeConfig;
use e2etest_tls::Tls;
use e2etest_tls::TlsExt;
use scylla::client::session::Session;
use scylla::client::session::TlsContext;
use scylla::client::session_builder::SessionBuilder;
use std::env;
use std::net::Ipv4Addr;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::LazyLock;
use tokio::sync::mpsc;
use tracing::info;
use uuid::Uuid;

struct Permanent;

struct Fixture {
    _dns: mpsc::Sender<Dns>,
    _tls: mpsc::Sender<Tls>,
    _db: mpsc::Sender<ScyllaCluster>,
    session: Session,
}

const DNS_IP_ADDR: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
const DB_PORT: u16 = 9042;
const SCYLLA_IP_ADDRS: [Ipv4Addr; 3] = [
    Ipv4Addr::new(127, 0, 0, 10),
    Ipv4Addr::new(127, 0, 0, 11),
    Ipv4Addr::new(127, 0, 0, 12),
];

fn default_scylla_args() -> Vec<String> {
    [
        "--overprovisioned",
        "--developer-mode=true",
        "--smp=2",
        "--memory=1G",
        "--unsafe-bypass-fsync=on",
        "--kernel-page-cache=on",
        "--reactor-backend=io_uring",
        "--rf-rack-valid-keyspaces=true",
        "--collectd=0",
        "--max-networking-io-control-blocks=1000",
        "--commitlog-use-o-dsync=0",
        "--flush-schema-tables-after-modification=false",
        "--auto-snapshot=0",
        "--logger-log-level=compaction=warn",
        "--logger-log-level=migration_manager=warn",
        "--shutdown-announce-in-ms=0",
        "--tablets-mode-for-new-keyspaces=enabled",
        "--alternator-ttl-period-in-seconds=0.5",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

static SUPERUSER_NAME: LazyLock<String> = LazyLock::new(|| Uuid::new_v4().simple().to_string());
static SUPERUSER_PASSWORD: LazyLock<String> = LazyLock::new(|| Uuid::new_v4().simple().to_string());
static SUPERUSER_SALTED_PASSWORD: LazyLock<String> = LazyLock::new(|| {
    bcrypt::hash(&*SUPERUSER_PASSWORD, bcrypt::DEFAULT_COST)
        .expect("failed to hash superuser password")
});

/// Builds the ScyllaDB config YAML with authentication enabled and superuser credentials set.
fn scylla_auth_config() -> Vec<u8> {
    let name = &*SUPERUSER_NAME;
    let salted = &*SUPERUSER_SALTED_PASSWORD;
    format!(
        "authenticator: PasswordAuthenticator\n\
         authorizer: CassandraAuthorizer\n\
         auth_superuser_name: '{name}'\n\
         auth_superuser_salted_password: '{salted}'"
    )
    .into_bytes()
}

impl e2etest::Fixture for Fixture {
    async fn setup(_setup: &mut impl e2etest::Setup) -> Option<Self> {
        info!("Creating dns server");
        let dns = e2etest_dns::new(DNS_IP_ADDR).await;

        info!("Creating tls actor");
        let tls = e2etest_tls::new(&SCYLLA_IP_ADDRS).await;

        info!("Creating scylla cluster router");
        let db = e2etest_scylla_cluster::new(
            "scylladb/scylla-nightly".into(),
            None,
            "/tmp".into(),
            false,
        )
        .await;

        let cert_path = tls.cert_path().await;
        let key_path = tls.key_path().await;
        let node_configs = SCYLLA_IP_ADDRS
            .iter()
            .map(|&ip| ScyllaNodeConfig {
                db_ip: ip,
                primary_vs_uris: vec![],
                secondary_vs_uris: vec![],
                args: default_scylla_args(),
                cert_path: Some(cert_path.clone()),
                key_path: Some(key_path.clone()),
                extra_config: Some(scylla_auth_config()),
            })
            .collect();
        db.start(node_configs).await;
        db.wait_for_ready().await;

        let tls_context = TlsContext::from(tls.client_tls_config().await);

        let session = SessionBuilder::new()
            .known_node_addr((SCYLLA_IP_ADDRS[0], DB_PORT).into())
            .user(SUPERUSER_NAME.as_str(), SUPERUSER_PASSWORD.as_str())
            .tls_context(Some(tls_context))
            .build()
            .await
            .expect("Failed to build session");
        Some(Fixture {
            _dns: dns,
            _tls: tls,
            _db: db,
            session,
        })
    }

    async fn teardown(self) {}
}

#[e2etest::test]
async fn exclusive1(fixture: Arc<Fixture>) {
    let result = fixture
        .session
        .query_unpaged(
            "CREATE KEYSPACE exclusive WITH replication = \
             { 'class': 'NetworkTopologyStrategy', 'replication_factor': 3 }",
            (),
        )
        .await;
    assert!(result.is_ok(), "Failed to create keyspace: {:?}", result);
}

#[e2etest::test]
async fn exclusive2(fixture: Arc<Fixture>) {
    let result = fixture
        .session
        .query_unpaged(
            "CREATE KEYSPACE exclusive WITH replication = \
             { 'class': 'NetworkTopologyStrategy', 'replication_factor': 3 }",
            (),
        )
        .await;
    assert!(result.is_ok(), "Failed to create keyspace: {:?}", result);
}

e2etest::group!(name = shared, fixtures = (Fixture));

#[e2etest::test(group = shared)]
async fn shared1(fixture: Arc<Fixture>) {
    let result = fixture
        .session
        .query_unpaged(
            "CREATE KEYSPACE shared1 WITH replication = \
             { 'class': 'NetworkTopologyStrategy', 'replication_factor': 3 }",
            (),
        )
        .await;
    assert!(result.is_ok(), "Failed to create keyspace: {:?}", result);
}

#[e2etest::test(group = shared)]
async fn shared2(fixture: Arc<Fixture>) {
    let result = fixture
        .session
        .query_unpaged(
            "CREATE KEYSPACE shared2 WITH replication = \
             { 'class': 'NetworkTopologyStrategy', 'replication_factor': 3}",
            (),
        )
        .await;
    assert!(result.is_ok(), "Failed to create keyspace: {:?}", result);
}

fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("install aws-lc-rs crypto provider");

    let filters = env::args().skip(1);
    if let Some(unshare_info) = e2etest::unshare_info() {
        let config = ConfigInUnshare::new(unshare_info)
            .with_permanent_fixture(Permanent)
            .with_concurrency(100);
        e2etest::run_in_unshare(config);
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
