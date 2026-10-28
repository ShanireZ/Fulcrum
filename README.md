# Fulcrum

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg?style=flat-square)](LICENSE)

<img src="assets/brand/fulcrum-logo.png" alt="Fulcrum" width="160" height="160">

A Rust web server, reverse proxy and load balancer.

- HTTP/1.1, HTTP/2 and HTTP/3 with automatic HTTPS.
- Reverse proxy, load balancing and TCP/UDP forwarding.
- Static files, caching, compression, JSON access logs and metrics.
- One process, one configuration and systemd-based upgrades.

## Build

Build from source with Docker for Linux:

```bash
git clone https://github.com/ShanireZ/Fulcrum.git
cd Fulcrum
docker build -f docker/Dockerfile.musl-product --target build -t fulcrum-build .
cid=$(docker create fulcrum-build) && docker cp "$cid:/fulcrum" ./fulcrum && docker rm "$cid"
```

The exported `fulcrum` is the musl static binary. See [build and test](docs/platform/build-and-test.md)
for prerequisites and the separate development gate.

## Use

Save this as `Fulcrumfile`:

```text
example.com {
    tls
    reverse_proxy 127.0.0.1:8080
}
```

```bash
./fulcrum validate Fulcrumfile
./fulcrum serve Fulcrumfile
```

## Documentation and license

[Configuration](docs/architecture/dsl-reference.md) · [Deployment](docs/platform/deploy.md) ·
[Documentation](docs/index.md) · [Product scope and status](PLAN.md)

[GNU GPL v3.0](LICENSE). The vendored Pingora fork retains its
[Apache-2.0 license](vendor/pingora/LICENSE) and [fork notices](vendor/pingora/FORK.md).
