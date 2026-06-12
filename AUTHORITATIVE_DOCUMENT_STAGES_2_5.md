# Authoritative document model: stages 2–5

This patch is the next compile-sized tranche after Stage 1. It implements model-safety changes that can be reviewed independently:

- semantic paragraph replacement preserves surviving IDs;
- only genuinely new paragraph IDs create CRDT records;
- removed IDs are removed explicitly;
- text replacement emits minimal UTF-8-safe delete/insert operations;
- granular mutations are prepared on an isolated fork;
- client replica imports prepared bytes only after authoritative ACK;
- paragraph order and paragraph-record sets must agree exactly;
- pending document edits are never evicted to admit newer edits.

Remaining architecture requiring separate patches after this tranche compiles and passes integration tests:

- durable on-disk canonical-intent outbox;
- semantic rebase after authoritative resnapshot;
- conflict quarantine/UI;
- explicit host projection-health state machine;
- transactional cloned-editor projection swap;
- paragraph/region-level projection fallback hierarchy;
- schema migration for removal of legacy inline runs from metadata;
- block/table canonical operations and CRDT schema;
- protocol feature negotiation;
- consistency hashes/checkpoints and crash-recovery journal;
- property/fuzz and multi-replica fault-injection suites;
- performance work: batching, multiple in-flight updates, compression.

These are intentionally not simulated by placeholder code. They require product policy and persistence/UI decisions beyond transport/model correctness.
