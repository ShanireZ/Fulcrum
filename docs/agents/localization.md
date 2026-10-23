---
type: convention
title: Localizing quoted owner words
description: When docs and comments are localized to Taiwan Traditional Chinese, owner words quoted verbatim change script only, never wording.
tags: [agents, localization]
status: stable
---

# Localizing quoted owner words

Docs and code comments in this repo use Taiwan Traditional Chinese. A localization pass converts both script and wording, with one exception.

## Quoted owner words change script only

Text that records what the owner said verbatim (`原話「…」`, quoted chat or pop-up answers in `PLAN.md` and commit-derived notes) is converted character by character and nothing else:

- `实现` becomes `實現`, not `實作`.
- `二进制` becomes `二進制`, not `二進位`.

The surrounding prose still uses Taiwan wording as usual.

**Why:** a decision row cites the owner's words as its evidence. Once the wording changes, the quotation marks no longer hold the owner's words. The 2026-10-17 localization (`ace76d5`) re-worded three such quotes (G159 once, G165 twice); the 2026-10-23 wording review restored them.
