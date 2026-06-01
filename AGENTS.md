Rust GPUI WYSIWIG maximally-performance-oriented word processor for competitive debate. You NEVER assume contexts for competitive debate, and you SHOULD always ask.

You SHOULD prefer gpui-component solutions to ground-up gpui solutions when a component is applicable. When no component is applicable or sufficient, you SHOULD either interact with the gpui-component vendor to fix it, or implement ground-up gpui only if the former fails. For the record: gpui-component is vendored on this project.

You SHOULD consider if there is a pre-existing crate to handle something. The 'search' tool is useful here. For deeper investigations into whether an unearthed crate meets your needs, deploy a Librarian subagent. If there are multiple options, you SHOULD ask while outlining pros and cons. You SHOULD use CLI cargo commands to handle crates and NEVER do it manually.

You SHOULD write readable and elegant code. You NEVER hack. You SHOULD keep the codebase aggressively modularized and crated when it is logical. If you find yourself pushing an individual file over 1,000 LOC, you SHOULD consider modularizing.

If you notice clear bugs while investigating something else, even if the bugs are unrelated to your main task, you SHOULD correct them. If the bug is unclear, you SHOULD use the 'ask' tool inquiring whether the behavior is intended.

If you are the main agent, the reviewer subagent, or the oracle subagent, you should use the 'rust_verify' tool when and only when ALL of your planned changes to the codebase are finished. If it returns an issue, correct it, then run it again until it doesn't return an issue. Do not use 'rust_verify heavy' unless specifically prompted to do so.

Be aggressive and ambitious in the vision for a working product.
