# Release blockers

The code and CI can be evaluated, but a public or multi-replica production
release is blocked by decisions or external systems that this repository
cannot safely invent.

## Owner action required

1. **Repository license.** `livylabs/resolver` does not publish an authoritative
   repository license. The owner must select one and add matching Cargo
   metadata before redistribution.
2. **Durable receipt adapter.** Implement and deploy a shared, tenant-scoped,
   expiring `ReceiptStoreFactory`, then build the default production binary
   with it. The included in-memory implementation is explicitly single-replica
   and non-durable.
3. **Atomic credit operation.** Extend the Livy backend with a durable
   reserve/capture/cancel operation bound to caller idempotency key and request
   fingerprint, including authoritative replay of the original resolver result.
   The current preflight-then-capture sequence has a documented race and cannot
   return the prior result after a completed retry.
4. **Remote Spider egress enforcement.** Deploy the policy proxy or Spider
   capability that enforces public-only DNS answers and every redirect hop at
   the machine making the request. Configure its same-origin authenticated
   readiness document; local URL checks alone cannot prove remote behavior.

The service fails closed or remains unready where it can do so without those
systems. Closing an item requires an integration test and an operations-runbook
update, not only a configuration change.
