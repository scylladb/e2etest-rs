# e2etest

The main crate for E2E test framework for Rust.

[![crates.io](https://img.shields.io/crates/v/e2etest.svg)](https://crates.io/crates/e2etest)
[![docs.rs](https://img.shields.io/docsrs/e2etest/latest)](https://docs.rs/e2etest)

## Getting Started

The user needs to define global fixture for all tests, initialization and
callback for test cases registration. The user needs to create a binary crate -
`e2etest` doesn't build directly into a binary.

The user can use the actors provided by `e2etest-rs` sub-crates, or create
their own actors. Most likely, the user wants to run dns server, firewall and
other testing actors - so the provided binary ought to be run in the unshared
environment.  `e2etest` provides the unshared environment directly, but on
specific distributions there must be need some setup to allow unshare to work.

The macros provided by `e2etest` don't require any additional dependencies in
your `Cargo.toml` file. However, `e2etest` uses internally `async-backtrace`
crate to provide better stack traces for async code. If you also want to use
`async-backtrace` in your own code, you need to add it explicitly to your
dependencies.


**Sample code for using `e2etest`**

```rust
mod sample {

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy)]
pub struct FixtureCfg {
    pub dns_ip: Ipv4Addr,
}

#[derive(Clone, Copy)]
pub struct FixtureOne {
    dns_ip: Ipv4Addr,
}

impl e2etest::Fixture for FixtureOne {
    async fn setup(setup: &mut impl e2etest::Setup) -> Option<Self> {
        let cfg = setup.get::<FixtureCfg>().await.unwrap();
        Some(Self { dns_ip: cfg.dns_ip })
    }

    async fn teardown(self) { }
}

#[derive(Clone, Copy)]
pub struct FixtureTwo {
    octet: u8,
}

impl e2etest::Fixture for FixtureTwo {
    async fn setup(setup: &mut impl e2etest::Setup) -> Option<Self> {
        let one = setup.setup::<FixtureOne>().await?;
        Some(Self { octet: one.dns_ip.octets()[2] })
    }

    async fn teardown(self) { }
}

#[derive(Clone, Copy)]
pub struct FixtureThree {
    number: usize,
}

impl e2etest::Fixture for FixtureThree {
    async fn setup(setup: &mut impl e2etest::Setup) -> Option<Self> {
        let two = setup.setup::<FixtureTwo>().await?;
        Some(Self { number: two.octet as usize * 1024 })
    }

    async fn teardown(self) { }
}

e2etest::group!(name = group, fixtures = (FixtureTwo));

#[e2etest::test(group = group, timeout = Duration::from_secs(5))]
async fn dns_ip_100(one: Arc<FixtureOne>, two: Arc<FixtureTwo>) {
    assert_eq!(one.dns_ip, Ipv4Addr::new(127, 0, 100, 1));
    assert_eq!(two.octet, 100);
}

#[e2etest::test(group = group)]
async fn dns_ip_200(one: Arc<FixtureOne>, _: Arc<e2etest::Skip>) {
    assert_eq!(one.dns_ip, Ipv4Addr::new(127, 0, 200, 1));
}

#[e2etest::test()]
async fn number_and_octet(two: Arc<FixtureTwo>, three: Arc<FixtureThree>) {
    assert_eq!(two.octet, 100);
    assert_eq!(three.number, 100 * 1024);
}

}

use std::net::Ipv4Addr;
use std::time::Duration;

tracing_subscriber::fmt::init();

if let Some(unshare_info) = e2etest::unshare_info() {
    let config = e2etest::ConfigUnshare::default()
            .with_permanent_fixture(sample::FixtureCfg { dns_ip: Ipv4Addr::new(127, 0, 100, 1) })
            .with_default_timeout(Duration::from_secs(10))
            .with_concurrency(10);
    e2etest::run_in_unshare(config, unshare_info);
    return;
}
let config = e2etest::Config::default()
    .with_concurrency(10);
let stats = e2etest::run(config);

assert!(stats.is_success());
assert_eq!(stats.tests_defined(), 3);
assert_eq!(stats.tests_included(), 3);
assert_eq!(stats.tests_launched(), 2);
assert_eq!(stats.tests_passed(), 2);
assert_eq!(stats.tests_skipped(), 1);
```

**Additional setup for Ubuntu**

In Ubuntu the user needs to register apparmor profile for the binary with tests
to allow unshare to work or disable apparmor for restrictions on user namespace
creation, or run the tests with sudo.

The following command disables apparmor for user namespace creation:

```bash
sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0
```

The following command registers the sample apparmor profile:

```bash
cat <<'EOF' | sudo apparmor_parser -r
abi <abi/4.0>,
include <tunables/global>

profile validator-bin /path/to/validator-bin flags=(unconfined) {
  userns,
}
EOF
```

## License

Licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
