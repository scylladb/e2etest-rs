# E2E test

This repository provides crates for E2E test framework for Rust.

[![crates.io](https://img.shields.io/crates/v/e2etest.svg)](https://crates.io/crates/e2etest)
[![docs.rs](https://img.shields.io/docsrs/e2etest/latest)](https://docs.rs/e2etest)

## Getting Started

The `e2etest-rs` repository provides building blocks for creating E2E tests.
See [e2etest](crates/e2etest/README.md) crate for more details on how to use
the framework.

## Components

- [e2etest](crates/e2etest/README.md): The main crate which provides the core
  functionality for E2E testing
- [e2etest-macros](crates/e2etest-macros/README.md): The crate which provides
  procedural macros
- [e2etest-chroot](crates/e2etest-chroot/README.md): The crate to manage chroot
  environments
- [e2etest-dns](crates/e2etest-dns/README.md): A DNS server
- [e2etest-firewall](crates/e2etest-firewall/README.md): A firewall emulator
- [e2etest-scylla-cluster](crates/e2etest-scylla-cluster/README.md): A ScyllaDB
  cluster manager
- [e2etest-scylla-proxy-cluster](crates/e2etest-scylla-proxy-cluster/README.md):
  A ScyllaDB Proxy cluster manager
- [e2etest-tls](crates/e2etest-tls/README.md): Utilities for handling TLS
- [e2etest-vector-store-cluster](crates/e2etest-vector-store-cluster/README.md):
  A Vector Store cluster manager

## License

Licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
