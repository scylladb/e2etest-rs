/*
 * Copyright 2025-present ScyllaDB
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

//! This crate provides a ScyllaDB cluster manager for e2etest tests. It provides an actor with
//! handler using `tokio::sync::mpsc::Sender` over `enum ScyllaCluster` message. It provides also a
//! `trait ScyllaClusterExt` with helper methods to send messages to the actor.

use async_backtrace::frame;
use async_backtrace::framed;
use e2etest_chroot::BindMount;
use e2etest_chroot::DockerRootfs;
use e2etest_chroot::Overlay;
use e2etest_chroot::OverlayGuard;
use e2etest_chroot::WithinChroot;
use std::ffi::OsStr;
use std::iter;
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Output;
use std::process::Stdio;
use std::time::Duration;
use tap::Tap;
use tempfile::TempDir;
use tokio::fs;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time;
use tracing::Instrument;
use tracing::debug;
use tracing::debug_span;
use tracing::info;

const DEFAULT_SCYLLA_CQL_PORT: u16 = 9042;

/// Configuration for a single ScyllaDB node in the test cluster.
#[derive(Clone)]
pub struct ScyllaNodeConfig {
    /// The IP address of this ScyllaDB node.
    pub db_ip: Ipv4Addr,
    /// Primary Vector Store URIs (--vector-store-primary-uri).
    pub primary_vs_uris: Vec<String>,
    /// Secondary Vector Store URIs (--vector-store-secondary-uri).
    pub secondary_vs_uris: Vec<String>,
    /// Args to pass to the ScyllaDB process.
    pub args: Vec<String>,
    /// Optional path to the TLS certificate file for client encryption.
    pub cert_path: Option<PathBuf>,
    /// Optional path to the TLS key file for client encryption.
    pub key_path: Option<PathBuf>,
    /// Optional extra YAML config to append to the ScyllaDB options file
    /// (e.g. authentication settings).
    pub extra_config: Option<Vec<u8>>,
}

/// Messages for the ScyllaDB cluster manager actor.
pub enum ScyllaCluster {
    Version {
        tx: oneshot::Sender<String>,
    },
    Start {
        node_configs: Vec<ScyllaNodeConfig>,
    },
    WaitForReady {
        tx: oneshot::Sender<bool>,
    },
    Stop {
        tx: oneshot::Sender<()>,
    },
    Up {
        node_configs: Vec<ScyllaNodeConfig>,
    },
    UpNode {
        node_config: ScyllaNodeConfig,
    },
    Down {
        tx: oneshot::Sender<()>,
    },
    DownNode {
        db_ip: Ipv4Addr,
        tx: oneshot::Sender<()>,
    },
    Flush {
        tx: oneshot::Sender<()>,
    },
}

/// Extension trait for `mpsc::Sender<ScyllaCluster>` to provide convenient async methods for
/// interacting with the ScyllaDB cluster manager actor.
pub trait ScyllaClusterExt {
    /// Returns the version of the ScyllaDB executable.
    fn version(&self) -> impl Future<Output = String>;

    /// Starts the ScyllaDB cluster with the given node configurations.
    fn start(&self, node_configs: Vec<ScyllaNodeConfig>) -> impl Future<Output = ()>;

    /// Stops the ScyllaDB cluster.
    fn stop(&self) -> impl Future<Output = ()>;

    /// Waits for the ScyllaDB cluster to be ready.
    fn wait_for_ready(&self) -> impl Future<Output = bool>;

    /// Starts a paused cluster back again.
    fn up(&self, node_configs: Vec<ScyllaNodeConfig>) -> impl Future<Output = ()>;

    /// Pauses a cluster.
    fn down(&self) -> impl Future<Output = ()>;

    /// Starts a single paused ScyllaDB instance back again.
    fn up_node(&self, node_config: ScyllaNodeConfig) -> impl Future<Output = ()>;

    /// Pauses a single ScyllaDB instance.
    fn down_node(&self, db_ip: Ipv4Addr) -> impl Future<Output = ()>;

    /// Restarts a single ScyllaDB instance.
    fn restart(&self, node_config: &ScyllaNodeConfig) -> impl Future<Output = ()>;

    /// Flushes all memtables to disk on all nodes.
    fn flush(&self) -> impl Future<Output = ()>;
}

impl ScyllaClusterExt for mpsc::Sender<ScyllaCluster> {
    #[framed]
    async fn version(&self) -> String {
        let (tx, rx) = oneshot::channel();
        self.send(ScyllaCluster::Version { tx })
            .await
            .expect("ScyllaClusterExt::version: internal actor should receive request");
        rx.await
            .expect("ScyllaClusterExt::version: internal actor should send response")
    }

    #[framed]
    async fn start(&self, node_configs: Vec<ScyllaNodeConfig>) {
        self.send(ScyllaCluster::Start { node_configs })
            .await
            .expect("ScyllaClusterExt::start: internal actor should receive request");
    }

    #[framed]
    async fn stop(&self) {
        let (tx, rx) = oneshot::channel();
        self.send(ScyllaCluster::Stop { tx })
            .await
            .expect("ScyllaClusterExt::stop: internal actor should receive request");
        rx.await
            .expect("ScyllaClusterExt::stop: internal actor should send response");
    }

    #[framed]
    async fn wait_for_ready(&self) -> bool {
        let (tx, rx) = oneshot::channel();
        self.send(ScyllaCluster::WaitForReady { tx })
            .await
            .expect("ScyllaClusterExt::wait_for_ready: internal actor should receive request");
        rx.await
            .expect("ScyllaClusterExt::wait_for_ready: internal actor should send response")
    }

    #[framed]
    async fn up(&self, node_configs: Vec<ScyllaNodeConfig>) {
        self.send(ScyllaCluster::Up { node_configs })
            .await
            .expect("ScyllaClusterExt::up: internal actor should receive request")
    }

    #[framed]
    async fn up_node(&self, node_config: ScyllaNodeConfig) {
        self.send(ScyllaCluster::UpNode { node_config })
            .await
            .expect("ScyllaClusterExt::up_node: internal actor should receive request")
    }

    #[framed]
    async fn down(&self) {
        let (tx, rx) = oneshot::channel();
        self.send(ScyllaCluster::Down { tx })
            .await
            .expect("ScyllaClusterExt::down: internal actor should receive request");
        rx.await
            .expect("ScyllaClusterExt::down: internal actor should send response");
    }

    #[framed]
    async fn down_node(&self, db_ip: Ipv4Addr) {
        let (tx, rx) = oneshot::channel();
        self.send(ScyllaCluster::DownNode { db_ip, tx })
            .await
            .expect("ScyllaClusterExt::down_node: internal actor should receive request");
        rx.await
            .expect("ScyllaClusterExt::down_node: internal actor should send response");
    }

    #[framed]
    async fn restart(&self, node_config: &ScyllaNodeConfig) {
        self.down_node(node_config.db_ip).await;
        self.up_node(node_config.clone()).await;
        assert!(self.wait_for_ready().await);
    }

    #[framed]
    async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        self.send(ScyllaCluster::Flush { tx })
            .await
            .expect("ScyllaClusterExt::flush: internal actor should receive request");
        rx.await
            .expect("ScyllaClusterExt::flush: internal actor should send response");
    }
}

/// Creates a new ScyllaDB cluster manager actor and returns a sender to send messages to it.
///
/// `target_name` can be either a path to the ScyllaDB executable or a Docker image name (with
/// optional tag). If `default_conf` is provided, it will be merged with extra config provided in
/// `ScyllaNodeConfig` when starting nodes.
#[framed]
pub async fn new(
    target_name: String,
    default_conf: Option<PathBuf>,
    tempdir: PathBuf,
    verbose: bool,
) -> mpsc::Sender<ScyllaCluster> {
    let (tx, mut rx) = mpsc::channel(10);

    assert!(tempdir.is_dir(), "tempdir '{tempdir:?}' is not a directory");

    let (target, docker_export) = prepare_target(target_name, &tempdir).await;
    if let Some(default_conf) = &default_conf {
        assert!(
            file_exists(default_conf).await,
            "scylla config '{default_conf:?}' does not exist"
        );
    }

    let mut state = State::new(target, default_conf, tempdir, verbose).await;

    tokio::spawn(
        frame!(async move {
            debug!("starting");

            while let Some(msg) = rx.recv().await {
                process(msg, &mut state).await;
            }

            info!("Final shutting down the scylla cluster...");
            stop(&mut state).await;

            if let Some(docker_export) = docker_export {
                docker_export.stop().await;
            }

            debug!("finished");
        })
        .instrument(debug_span!("db")),
    );

    tx
}

enum Target {
    Binary(PathBuf),
    ContainerRoot { rootfs: PathBuf, overlay: TempDir },
}

impl Target {
    const CONTAINER_CERT_PATH: &'static str = "/e2etest/cert.pem";
    const CONTAINER_KEY_PATH: &'static str = "/e2etest/key.pem";

    fn cert_key<'a>(&self, node_config: &'a ScyllaNodeConfig) -> Option<(&'a Path, &'a Path)> {
        match self {
            Target::Binary(_) => {
                if let (Some(cert), Some(key)) = (&node_config.cert_path, &node_config.key_path) {
                    Some((cert, key))
                } else {
                    None
                }
            }
            Target::ContainerRoot { .. } => {
                if let (Some(_), Some(_)) = (&node_config.cert_path, &node_config.key_path) {
                    Some((
                        Path::new(Self::CONTAINER_CERT_PATH),
                        Path::new(Self::CONTAINER_KEY_PATH),
                    ))
                } else {
                    None
                }
            }
        }
    }
}

async fn prepare_target(name: String, tempdir: &Path) -> (Target, Option<DockerRootfs>) {
    let path = PathBuf::from(&name);
    if executable_exists(&path).await {
        (Target::Binary(path), None)
    } else {
        let (name, tag) = e2etest_chroot::extract_docker_name_tag(&name);
        let overlay = TempDir::new_in(tempdir)
            .expect("failed to create temporary directory for container overlay");
        let docker_rootfs = DockerRootfs::new(name, tag, tempdir).await;
        (
            Target::ContainerRoot {
                rootfs: docker_rootfs.rootfs_path(),
                overlay,
            },
            Some(docker_rootfs),
        )
    }
}

impl Target {
    async fn scylla(
        &self,
        args: impl IntoIterator<Item = impl Into<String> + AsRef<OsStr>>,
    ) -> Output {
        let (mut cmd, _overlay) = match self {
            Target::Binary(path) => {
                let mut cmd = Command::new(path);
                cmd.args(args);
                (cmd, None)
            }
            Target::ContainerRoot {
                rootfs,
                overlay: tempdir,
            } => {
                let (overlay, overlay_guard) = Overlay::new(tempdir.path()).await;
                let mut cmd = Command::new("/usr/bin/scylla");
                cmd.args(args).within_chroot(overlay, rootfs, iter::empty());
                (cmd, Some(overlay_guard))
            }
        };

        cmd.kill_on_drop(true);

        match cmd.output().await {
            Ok(output) => output,
            Err(err) => {
                panic!(
                    "scylla: {err}",
                    err = e2etest_chroot::describe_command_error(err)
                );
            }
        }
    }

    async fn run(
        &self,
        node_config: &ScyllaNodeConfig,
        workdir: &Path,
        confdir: &Path,
        conffile: &Path,
        seeds: &str,
        verbose: bool,
    ) -> Child {
        let db_ip = node_config.db_ip;
        let mut args = vec![
            "--listen-address".to_string(),
            db_ip.to_string(),
            "--rpc-address".to_string(),
            db_ip.to_string(),
            "--api-address".to_string(),
            db_ip.to_string(),
            "--log-to-stdout".to_string(),
            "true".to_string(),
            "--logger-ostream-type".to_string(),
            "stdout".to_string(),
            "--endpoint-snitch".to_string(),
            "GossipingPropertyFileSnitch".to_string(),
        ]
        .tap_mut(|args| {
            if !node_config.primary_vs_uris.is_empty() {
                args.extend([
                    "--vector-store-primary-uri".to_string(),
                    node_config.primary_vs_uris.join(","),
                ]);
            }
        })
        .tap_mut(|args| {
            if !node_config.secondary_vs_uris.is_empty() {
                args.extend([
                    "--vector-store-secondary-uri".to_string(),
                    node_config.secondary_vs_uris.join(","),
                ]);
            }
        })
        .tap_mut(|args| {
            if !seeds.is_empty() {
                args.extend([
                    "--seed-provider-parameters".to_string(),
                    format!("seeds={seeds}"),
                ]);
            }
        })
        .tap_mut(|args| {
            args.extend(node_config.args.clone());
        });

        let (mut cmd, overlay) = match self {
            Target::Binary(path) => {
                args.extend([
                    "--options-file".to_string(),
                    conffile.to_string_lossy().to_string(),
                    "--workdir".to_string(),
                    workdir.to_string_lossy().to_string(),
                ]);

                let mut cmd = Command::new(path);
                cmd.env("SCYLLA_CONF", confdir);
                (cmd, None)
            }

            Target::ContainerRoot {
                rootfs,
                overlay: tempdir,
            } => {
                let (overlay, overlay_guard) = Overlay::new(tempdir.path()).await;
                let mut mounts = vec![
                    BindMount::new(workdir, Path::new("/e2etest/workdir")),
                    BindMount::new(confdir, Path::new("/e2etest/confdir")),
                    BindMount::new(conffile, Path::new("/e2etest/conffile")),
                ];
                args.extend([
                    "--options-file".to_string(),
                    "/e2etest/conffile".to_string(),
                    "--workdir".to_string(),
                    "/e2etest/workdir".to_string(),
                ]);
                if let Some((container_cert, container_key)) = self.cert_key(node_config)
                    && let (Some(cert), Some(key)) = (&node_config.cert_path, &node_config.key_path)
                {
                    mounts.extend([
                        BindMount::new(cert.as_path(), container_cert),
                        BindMount::new(key.as_path(), container_key),
                    ]);
                }

                let mut cmd = Command::new("/usr/bin/scylla");
                cmd.within_chroot(overlay, rootfs, mounts);
                cmd.env("SCYLLA_CONF", "/e2etest/confdir");
                (cmd, Some(overlay_guard))
            }
        };
        if !verbose {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        let child = cmd
            .args(args)
            .kill_on_drop(true)
            .spawn()
            .expect("start: failed to spawn scylladb");
        Child {
            child,
            _overlay: overlay,
        }
    }
}

struct Child {
    child: tokio::process::Child,
    _overlay: Option<OverlayGuard>,
}

impl Child {
    async fn kill(mut self) {
        self.child
            .start_kill()
            .expect("kill: failed to send SIGKILL to scylladb process");
        self.child
            .wait()
            .await
            .expect("kill: failed to wait for scylladb process to exit");
    }
}

struct NodeState {
    db_ip: Ipv4Addr,
    child: Option<Child>,
    workdir: Option<TempDir>,
}

struct State {
    target: Target,
    default_conf: Option<PathBuf>,
    tempdir: PathBuf,
    nodes: Vec<NodeState>,
    version: String,
    verbose: bool,
}

impl State {
    #[framed]
    async fn new(
        target: Target,
        default_conf: Option<PathBuf>,
        tempdir: PathBuf,
        verbose: bool,
    ) -> Self {
        let version = String::from_utf8_lossy(&target.scylla(["--version"]).await.stdout)
            .trim()
            .to_string();

        Self {
            target,
            default_conf,
            version,
            tempdir,
            nodes: Vec::new(),
            verbose,
        }
    }
}

#[framed]
async fn process(msg: ScyllaCluster, state: &mut State) {
    match msg {
        ScyllaCluster::Version { tx } => {
            tx.send(state.version.clone())
                .expect("process ScyllaCluster::Version: failed to send a response");
        }

        ScyllaCluster::Start { node_configs } => {
            start(node_configs, state).await;
        }

        ScyllaCluster::Stop { tx } => {
            stop(state).await;
            tx.send(())
                .expect("process ScyllaCluster::Stop: failed to send a response");
        }

        ScyllaCluster::WaitForReady { tx } => {
            tx.send(wait_for_ready(state).await)
                .expect("process ScyllaCluster::WaitForReady: failed to send a response");
        }

        ScyllaCluster::Up { node_configs } => {
            up(node_configs, state).await;
        }

        ScyllaCluster::UpNode { node_config } => {
            up_node(node_config, state).await;
        }

        ScyllaCluster::Down { tx } => {
            down(state).await;
            tx.send(())
                .expect("process ScyllaCluster::Down: failed to send a response");
        }

        ScyllaCluster::DownNode { db_ip, tx } => {
            down_node(state, db_ip).await;
            tx.send(())
                .expect("process ScyllaCluster::DownNode: failed to send a response");
        }

        ScyllaCluster::Flush { tx } => {
            flush(state).await;
            tx.send(())
                .expect("process ScyllaCluster::Flush: failed to send a response");
        }
    }
}

#[framed]
async fn run_node(
    node_config: &ScyllaNodeConfig,
    seeds: &str,
    path: &Path,
    rack: &str,
    state: &State,
) -> Child {
    let cert_key = state.target.cert_key(node_config);

    let mut config = if let Some(default_conf) = &state.default_conf {
        fs::read(default_conf)
            .await
            .expect("start: failed to read default scylla config")
    } else {
        Vec::new()
    };
    if let Some((cert, key)) = cert_key {
        if !config.is_empty() {
            config.push(b'\n');
        }
        config.extend_from_slice(
            format!(
                r#"client_encryption_options:
  enabled: true
  certificate: {cert}
  keyfile: {key}"#,
                cert = cert.display(),
                key = key.display()
            )
            .as_bytes(),
        );
    }
    if let Some(extra) = &node_config.extra_config {
        if !config.is_empty() {
            config.push(b'\n');
        }
        config.extend_from_slice(extra);
    }
    config.push(b'\n');
    let conf = path.join("scylla.conf");
    fs::write(&conf, config)
        .await
        .expect("start: failed to write scylla config");

    let conf_dir = path.join("conf");
    fs::create_dir_all(&conf_dir)
        .await
        .expect("start: failed to create conf directory");

    let rack_dc_properties = format!("dc=datacenter1\nrack={rack}\nprefer_local=true\n");
    let properties_path = conf_dir.join("cassandra-rackdc.properties");
    debug!(
        "Creating cassandra-rackdc.properties at {:?}",
        properties_path
    );

    fs::write(&properties_path, &rack_dc_properties)
        .await
        .expect("start: failed to write cassandra-rackdc.properties");

    state
        .target
        .run(node_config, path, &conf_dir, &conf, seeds, state.verbose)
        .await
}

#[framed]
async fn start(node_configs: Vec<ScyllaNodeConfig>, state: &mut State) {
    if node_configs.is_empty() {
        return;
    }

    let db_ips: Vec<Ipv4Addr> = node_configs.iter().map(|c| c.db_ip).collect();
    debug!("scylla_cluster: using DB IPs: {:?}", db_ips);

    // The first node IP will be used as seed for whole cluster
    let seed_ip = db_ips.first().unwrap();

    // Start each node sequentially with proper seed configuration
    for (i, node_config) in node_configs.iter().enumerate() {
        let workdir = TempDir::new_in(&state.tempdir)
            .expect("start: failed to create temporary directory for scylladb");
        let rack = format!("rack{}", i + 1);
        let seeds = seed_ip.to_string();

        info!(
            "Starting Scylla node {} on IP {} in {} (primary: {:?}, secondary: {:?})",
            i + 1,
            node_config.db_ip,
            rack,
            node_config.primary_vs_uris,
            node_config.secondary_vs_uris
        );

        let child = run_node(node_config, &seeds, workdir.path(), &rack, state).await;

        state.nodes.push(NodeState {
            db_ip: node_config.db_ip,
            child: Some(child),
            workdir: Some(workdir),
        });
    }

    debug!(
        "Started {} Scylla nodes in {}-rack cluster configuration, waiting for initialization...",
        state.nodes.len(),
        state.nodes.len()
    );
}

#[framed]
async fn stop(state: &mut State) {
    for node in &mut state.nodes {
        if let Some(child) = node.child.take() {
            child.kill().await;
        }
        node.workdir = None;
    }
    state.nodes.clear();
}

#[framed]
async fn down(state: &mut State) {
    for node in &mut state.nodes {
        if let Some(child) = node.child.take() {
            child.kill().await;
        }
    }
}

#[framed]
async fn down_node(state: &mut State, db_ip: Ipv4Addr) {
    if let Some(node) = state.nodes.iter_mut().find(|n| n.db_ip == db_ip)
        && let Some(child) = node.child.take()
    {
        child.kill().await;
    }
}

#[framed]
async fn wait_for_node(state: &State, ip: Ipv4Addr) -> bool {
    loop {
        if String::from_utf8_lossy(
            &state
                .target
                .scylla(["nodetool", "-h", &ip.to_string(), "status"])
                .await
                .stdout,
        )
        .lines()
        .any(|line| line.starts_with(&format!("UN {ip}")))
        {
            loop {
                if is_cql_port_ready(ip).await {
                    return true;
                }
                time::sleep(Duration::from_millis(100)).await;
            }
        }
        time::sleep(Duration::from_millis(100)).await;
    }
}

#[framed]
async fn is_cql_port_ready(ip: Ipv4Addr) -> bool {
    use std::net::SocketAddr;
    use tokio::net::TcpStream;

    let addr = SocketAddr::from((ip, DEFAULT_SCYLLA_CQL_PORT));

    match tokio::time::timeout(Duration::from_millis(500), TcpStream::connect(addr)).await {
        Ok(Ok(_)) => {
            debug!("CQL port {} is open on {}", DEFAULT_SCYLLA_CQL_PORT, ip);
            true
        }
        _ => {
            debug!(
                "CQL port {} is not yet ready on {}",
                DEFAULT_SCYLLA_CQL_PORT, ip
            );
            false
        }
    }
}

#[framed]
/// Waits for ScyllaDB to be ready by checking the nodetool status.
async fn wait_for_ready(state: &State) -> bool {
    if state.nodes.is_empty() {
        tracing::error!("No ScyllaDB nodes to wait for - nodes list is empty");
        return false;
    }

    for node in &state.nodes {
        tracing::info!(
            "Waiting for ScyllaDB node at IP {} to be ready...",
            &node.db_ip
        );
        wait_for_node(state, node.db_ip).await;
        tracing::info!("ScyllaDB node at IP {} is ready.", &node.db_ip);
    }

    true
}

#[framed]
async fn up(node_configs: Vec<ScyllaNodeConfig>, state: &mut State) {
    if state.nodes.is_empty() {
        return;
    }

    // Use the first node as seed for whole cluster
    let seed_ip = state.nodes.first().unwrap().db_ip.to_string();

    let nodes = state
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (i, node.workdir.as_ref().unwrap().path().to_path_buf()))
        .collect::<Vec<_>>();

    for (i, path) in nodes.into_iter() {
        let rack = format!("rack{}", i + 1);
        let child = run_node(&node_configs[i], &seed_ip, &path, &rack, state).await;
        state.nodes[i].child = Some(child);
    }
}

#[framed]
async fn up_node(node_config: ScyllaNodeConfig, state: &mut State) {
    if state.nodes.is_empty() {
        return;
    }

    // Use the first node as seed
    let seed_ip = state.nodes.first().unwrap().db_ip.to_string();

    if let Some((i, node)) = state
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.db_ip == node_config.db_ip)
    {
        let rack = format!("rack{}", i + 1);
        let path = node.workdir.as_ref().unwrap().path().to_path_buf();
        let child = run_node(&node_config, &seed_ip, &path, &rack, state).await;
        state.nodes[i].child = Some(child);
    }
}

#[framed]
async fn flush(state: &State) {
    for node in &state.nodes {
        info!("Flushing node {}", node.db_ip);
        let output = state
            .target
            .scylla(["nodetool", "-h", &node.db_ip.to_string(), "flush"])
            .await;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("nodetool flush failed on {}: {}", node.db_ip, stderr);
        } else {
            info!("Flush completed on node {}", node.db_ip);
        }
    }
}

/// Checks if the file exists and is executable.
#[framed]
async fn executable_exists(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path).await else {
        return false;
    };
    metadata.is_file() && (metadata.permissions().mode() & 0o111 != 0)
}

/// Checks if the file exists.
#[framed]
async fn file_exists(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path).await else {
        return false;
    };
    metadata.is_file()
}
