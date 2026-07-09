# flowstate-autoflow

Sandbox crate for Flowstate's autoflow pipeline.

This crate owns the data contracts for:

- ASR word streams with timestamps and confidence.
- Debate row-boundary predictions.
- Structured flow row events.
- Future deterministic routing into Flowstate flow documents.

It intentionally does not contain GPUI code or a concrete ASR runtime. Model
runtimes can be added behind explicit adapters once the training/export path is
settled.
