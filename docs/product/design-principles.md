---
type: product
title: Design principles
description: How the authoritative product principles constrain changes.
---
# Design principles

Read [PLAN §4](../../PLAN.md). In design reviews, test whether the proposal keeps one process/configuration/observability system; retains one persistent expected-state authority; makes temporary exceptions visible; validates atomically before activation; preserves secure defaults; and supports reproducible performance claims.

Requiring an auxiliary installation, adding persistent runtime writes, hiding overrides or treating partial load as success needs resolution against those principles. The removed fallback layer remains an example of temporary scaffolding whose success criterion was deletion.
