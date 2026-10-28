# Architecture baseline

These documents contain the approved technical baseline; [PLAN.md](../../PLAN.md) controls scope and decisions.

- [Process model](process-model.md): components, systemd lifecycle and threads.
- [Configuration](configuration.md): DSL, JSON and runtime graph.
- [Control plane](control-plane.md): atomic load and temporary overrides.
- [Data path](data-path.md): shared HTTP routing, L4, files and cache.
- [TLS](tls.md): certificate selection, ACME and credentials.
- [DSL reference](dsl-reference.md): complete configuration contract and examples.
- [Observability](observability.md): access-log fields, metrics and stats.
- [Removed fallback](fallback.md): migration behavior and retained constraints.
