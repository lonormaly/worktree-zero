# Working on Worktree Zero

Worktree Zero manages source trees and generated runtime state. Safety is part of the product.

- Never delete, clean, reset, stash, or overwrite a user's working files.
- Resolve exact worktree and runtime identities before cleanup.
- Refuse dirty worktrees, live processes, broad paths, and unmarked runtime directories.
- Share immutable/content-addressed data only. Mutable state stays isolated.
- Treat physical volume allocation and logical directory size as separate measurements.
- Mark behavior as measured, implemented, or proposed. Do not turn one into another in documentation.
- Prefer integration with a proven OSS primitive over reimplementing it without evidence.
- Keep generic behavior here. Design-partner vocabulary and product policy stay in the consuming repository.
- Add a failure test for every destructive or lifecycle guard.
