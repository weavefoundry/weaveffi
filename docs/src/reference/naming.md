## Naming and Package Conventions

This guide standardizes how we name the Weave projects, repositories, packages, modules, and identifiers across ecosystems.

### Human-facing brand names (prose)

- Use condensed names in sentences and documentation:
  - WeaveFFI
  - WeaveHeap

### Repository and package slugs (URLs and registries)

- Use condensed lowercase slugs for top-level repositories and published packages:
  - GitHub: `weaveffi`, `weaveheap` (repos: `weavefoundry/weaveffi`, `weavefoundry/weaveheap`)
  - crates.io (top-level crates): `weaveffi`, `weaveheap`
  - npm: `@weavefoundry/weaveffi`, `@weavefoundry/weaveheap`
  - PyPI: `weaveffi`, `weaveheap`
  - SPM (repo slug): `weaveffi`, `weaveheap`

- Use hyphenated slugs for subpackages and components, prefixed with the top-level slug:
  - Examples: `weaveffi-core`, `weaveffi-ir`, `weaveheap-core`

Rationale: condensed top-level slugs unify handles across registries and are ergonomic to type; hyphenated subpackages remain idiomatic and map cleanly to ecosystems that normalize to underscores or CamelCase.

### Code identifiers by ecosystem

- Rust
  - Crates: hyphenated subcrates on crates.io (e.g., `weaveffi-core`), imported as underscores (e.g., `weaveffi_core`). Top-level crate (if any): `weaveffi`.
  - Modules/paths: snake_case.
  - Types/traits/enums: CamelCase (e.g., `WeaveFFI`).

- Swift / Apple platforms
  - Package products and modules: UpperCamelCase (e.g., `WeaveFFI`, `WeaveHeap`).
  - Keep repo slug condensed; SPM product name provides the CamelCase surface.

- Java / Kotlin (Android)
  - Group ID / package base: reverse-DNS, all lowercase (e.g., `com.weavefoundry.weaveffi`).
  - Artifact ID: top-level condensed (e.g., `weaveffi`); sub-artifacts hyphenated (e.g., `weaveffi-android`).
  - Class names: UpperCamelCase (e.g., `WeaveFFI`).

- JavaScript / TypeScript (Node, bundlers)
  - Package name: scope + condensed for top-level, hyphenated for subpackages (e.g., `@weavefoundry/weaveffi`, `@weavefoundry/weaveffi-core`).
  - Import alias: flexible, prefer `WeaveFFI` in examples when using default exports or named namespaces.

- Python
  - PyPI name: top-level condensed (e.g., `weaveffi`); subpackages hyphenated (e.g., `weaveffi-core`).
  - Import module: condensed for top-level (e.g., `import weaveffi`); underscores for hyphenated subpackages (e.g., `import weaveffi_core`).

- C / CMake
  - Target/library names: snake_case (e.g., `weaveffi`, `weaveffi_core`).
  - Header guards / include dirs: snake_case or directory-based (e.g., `#include <weaveffi/weaveffi.h>`).

### Writing guidelines

- In prose, prefer the condensed brand names: “WeaveFFI”, “WeaveHeap”.
- In code snippets, follow the host language conventions above.
- For cross-language docs, show both the repo/package slug and the language-appropriate identifier on first mention, e.g., “Install `weaveffi` (import as `weaveffi`, Swift module `WeaveFFI`). For subpackages, install `weaveffi-core` (import as `weaveffi_core`).”

### Migration guidance

- Rename existing repositories to `weaveffi` and `weaveheap` (GitHub will auto-redirect; update docs/badges).
- New crates and packages should follow the condensed top-level + hyphenated subpackage pattern:
  - Rust crates: `weaveffi-*`, `weaveheap-*`.
  - npm packages: `@weavefoundry/weaveffi-*`, `@weavefoundry/weaveheap-*`.
  - Swift products: UpperCamelCase (e.g., `WeaveFFICore`).
- Prefer condensed top-level slugs. Avoid hyphenated top-level slugs like `weave-ffi`, `weave-heap` going forward.

### Examples

- Rust
  - Crate: `weaveffi-core`
  - Import: `use weaveffi_core::{WeaveFFI};`

- Swift (SPM)
  - Repo: `weaveffi`
  - Package product: `WeaveFFI`
  - Import: `import WeaveFFI`

- Python
  - Package: `weaveffi`
  - Import: `import weaveffi as ffi`

- Node
  - Package: `@weavefoundry/weaveffi`
  - Import: `import { WeaveFFI } from '@weavefoundry/weaveffi'`
