Rust GPUI WYSIWIG maximally-performance-oriented word processor for competitive debate. You NEVER assume contexts for competitive debate, and you SHOULD always ask.

You SHOULD prefer gpui-component solutions to ground-up gpui solutions when a component is applicable. When no component is applicable or sufficient, you SHOULD either interact with the gpui-component vendor to fix it, or implement ground-up gpui only if the former fails. For the record: gpui-component is vendored on this project.

You SHOULD consider if there is a pre-existing crate to handle something. The 'search' tool is useful here. For deeper investigations into whether an unearthed crate meets your needs, deploy a Librarian subagent. If there are multiple options, you SHOULD ask while outlining pros and cons. You SHOULD use CLI cargo commands to handle crates and NEVER do it manually.

You SHOULD write readable and elegant code. You NEVER hack. You SHOULD keep the codebase aggressively modularized and crated when it is logical. If you find yourself pushing an individual file over 1,000 LOC, you SHOULD consider modularizing.

If you notice clear bugs while investigating something else, even if the bugs are unrelated to your main task, you SHOULD correct them. If the bug is unclear, you SHOULD use the 'ask' tool inquiring whether the behavior is intended.

When you need to check something against Rust documentation or look for an API, ALWAYS deploy the Librarian subagent. This agents instruction is sufficient permission for you to deploy the subagent, do NOT fail to follow it on account of expecting a subagent request to appear explicitly in the prompt. You NEVER invent APIs. 

These subagents SHOULD be deployed WITHOUT explicit human request when a task arises that they fit: explore, librarian, task, and quick_task. NOT deploying these subagents when a task fits their use-case is a FAILURE and MUST be avoided.

These subagents should NEVER be deployed without an explicit request for subagent deployment from the human: plan, reviewer, and oracle. Deploying these subagents when they were not explicitly requested is a FAILURE and MUST be avoided.

If you are the main agent, the reviewer subagent, or the oracle subagent, you should use the 'rust_verify' tool when and only when ALL of your planned changes to the codebase are finished. If it returns an issue, correct it, then run it again until it doesn't return an issue.
