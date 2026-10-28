---
type: architecture
title: Removed fallback layer
description: Migration behavior and durable constraints after G98.
---
# Removed fallback layer

This is the approved technical baseline and yields to [PLAN.md](../../PLAN.md) on conflict.


G98 removed the complete Caddy/nginx fallback once L4, files and cache were native. Removed types, forwarding branches and fallback_routes fields must stay removed; a permanently empty field would falsely imply a supported path. `fallback_nginx` and `fallback_caddy` reject with dedicated diagnostic FUL-DSL-0034, explaining removal rather than suggesting a spelling error.

The historical layer forwarded files/cache to nginx and other incomplete capabilities to Caddy through ordinary ProxyTarget/shared reverse_proxy code, without translating capabilities or configuration semantics. Existing systemd units used Wants/After rather than Requires, so a failed backend did not kill the entrance. Fulcrum never started, supervised, restarted or read external engine configuration, and never statically linked the engines.

Retained constraints:

1. Unconfigured capability (501) and configured but unreachable backend (502) describe different faults.
2. Report unused/dead configuration; its presence in a file does not prove it is active.
3. Reuse a single forwarding implementation for rewrite, header changes and address fallback; cache continues through proxy_with rather than another proxy path.
4. Preserve self-loop warnings after removing their original fallback consumer. Runtime::self_loop_warnings returns diagnostics without logging; log_load_summary emits them on the server side. Report only when both the host is a local form and the port belongs to this process's listeners; false positives must not reject valid configuration. These are warnings because self TLS-to-cleartext proxying can be intentional. Walk nested containers, name the affected upstream exactly once and exclude unrelated upstreams. Test warnings without generating actual loop traffic.
