---
type: product
title: First-release scope
description: Navigation to included and excluded product capabilities.
---
# First-release scope

[PLAN §6](../../PLAN.md) owns the capability surface and exclusions. Native L4, files, complete HTTP cache and HTTP/3 integration accompany the reverse proxy. Protocol stacks remain audited library code; HTTP/3 integration does not authorize implementing QUIC/QPACK.

The former Caddy/nginx fallback is removed (G98); old fallback options receive a dedicated migration diagnostic. Scope and milestone exits are different: consult [PLAN §7](../../PLAN.md) for operational acceptance and [DSL](../architecture/dsl-reference.md) for supported configuration.
