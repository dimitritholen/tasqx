---
description: Why a failed charge is never retried automatically
---
# Payment retries

Ruling: a failed charge is never retried by the worker. The customer retries,
with the same idempotency key, from the checkout page.

Why: the double charge in March came from a retry that raced a slow success.
An idempotency key makes a second attempt safe; an automatic one makes it likely.
