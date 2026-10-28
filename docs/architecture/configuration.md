---
type: architecture
title: Configuration layers
description: DSL compiles to JSON and a validated runtime graph.
---
# Configuration layers

This is the approved technical baseline and yields to [PLAN.md](../../PLAN.md) on conflict.


The direction is DSL → structured JSON → validated runtime graph (site index, matchers, execution chain, upstream pools, TLS resolvers and cache handles). Both DSL and JSON are public inputs. The JSON layer is the sole internal configuration fact for diffs, versioning and rollback (G11/G48). There is no JSON-to-DSL generation: comments, indentation and shorthand would not survive a round trip.

JSON provides unambiguous types and stable machine-generated diffs without YAML's implicit conversions or TOML's awkward nested route arrays. The load API accepts the same format as the on-disk structured representation; no wrapper envelope. Pingora's own conf/*.yaml ServerConf is a separate, generated framework input.

`fulcrum validate` checks diagnostics, structured configuration, runtime graph and TLS loading. `compile` emits JSON; `plan` explains ordered steps; `serve` activates the graph. `Runtime::build()` validates the public JSON route independently of the DSL frontend, including values that are lexically valid DSL words but semantically invalid CIDRs or identifiers.

The `fulcrum-config` sources own mechanics: directive.rs (`chain_directives!` with explicit order), compile.rs (stable site-top-level ordering and order-preserving route blocks), diag.rs (stable FUL-DSL diagnostics), placeholder.rs (position validation) and model.rs (serde JSON, including null for None).

G49's built-in order has four required companions: a published [order table](dsl-reference.md#3-execution-order); `route` as an order-preserving escape; compile-time exhaustive placement and handling of new directives; and diagnostics explaining the actual execution position. `doc_contract.rs` compares table rows with code, runtime routing tests check semantics, and real serving tests check responses. Unit routing logic remains independent of Pingora; this costs a RequestHeader-to-RequestCtx conversion and enables isolated tests. Neither layer alone proves the other.

See [DSL](dsl-reference.md), [control plane](control-plane.md) and [process model](process-model.md).
