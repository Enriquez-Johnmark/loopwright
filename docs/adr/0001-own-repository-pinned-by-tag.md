# The engine lives in its own repository, and products pin it by tag

The engine (released as Loopwright, renamed Nudgewright on 2026-10-24) was built inside Kompleto, its first product, behind a one-way boundary that a CI check enforced (Kompleto ADR 0001). When the owner decided to build five more products, each in its own public repository for the portfolio, the engine moved to its own repository. Its history came with it (`git subtree split`), so its development stays visible.

Each product depends on a tagged release (`tag = "v0.1.0"`). A product upgrades when it chooses, so an engine change made for one product can never break another.

## Considered options

- **One repository with a Cargo workspace (engine plus every product):** simpler to change the engine and every product in one commit, but it can't give each product its own public repository, which the owner wants.
- **Publishing to crates.io:** unnecessary while only the owner's products use it; a git tag gives the same pinning.
- **Products track `main` instead of tags:** one unfinished engine change would reach every product at once.

## Consequences

- An engine change takes two steps: release a new tag, then upgrade each product that needs it.
- The engine names no product (CI enforces it), and owns only its own tables.
- The crash test that kills a real program mid-send lives in each product, since only a product has a program to kill; the engine keeps the `failpoints` feature for it.
