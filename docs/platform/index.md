# Platform and operations

These pages map authoritative [PLAN.md](../../PLAN.md) decisions to technical/contributor contracts. Source/configuration owns exact versions and executable criteria; historical operation records remain outside Git.

- [Technology stack](tech-stack.md) — language, Pingora, quiche, TLS, configuration, distribution.
- [Dependency policy](dependency-policy.md) — quarantine, dual-lock adoption, monitoring, freeze rules.
- [Supply chain](supply-chain.md) — activation, reachability, advisories, symbols, notices.
- [HTTP/3 integration](http3-libraries.md) — libraries, DCID ownership, handover, independent-client evidence.
- [Upstream contribution](upstream-pr.md) — scope, prerequisites, validation, attribution, publication.
- [Build and test](build-and-test.md) — Docker, full/scoped gates, caches, hooks.
- [Host/gate traps](host-and-gate-traps.md) — flags, ports, cleanup, shell inventory, limitations.
- [Deployment](deploy.md) — systemd, atomic load, binary replacement, acceptance.
- [Security](security-baseline.md) — admission, secrets, privileges, libraries, fixture limits.
