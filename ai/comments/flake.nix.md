# flake.nix

- The single package definition for `ai` (devenv is only the dev shell). Install and update:
  - `nix profile add /hatimthayyil/code/ai-tools#ai`
  - `nix profile upgrade ai`
  - Build locally: `nix build` → `./result/bin/ai`
- A local path with `.git` is fetched as `git+file`, so only git-tracked files reach the store (`target/` never does). New files must be `git add`ed before nix sees them. `src` is further narrowed with `lib.fileset` to the Rust sources, so doc edits do not trigger rebuilds.
- The Claude Code plugin ships with the binary: `postInstall` copies `plugins/claude` (git-tracked files only, so not the engine's generated types) to `$out/share/ai/plugins/claude`; `nix profile` links it at `~/.nix-profile/share/ai/plugins/claude`, inside the marketplace at `share/ai/plugins` that installs it (README, Claude Code plugin). A plugin edit rebuilds the package; the plugin is a few files, so a second derivation is not worth it. An interactive session loads and runs it from the read-only store path; the engine's type declarations are then simply not written (checked 2026-10-15).
- `rustPlatform.buildRustPackage` with `cargoLock.lockFile`: no generated Nix, no extra inputs. Replaced devenv's crate2nix-based `languages.rust.import`. nixos-unstable's rustc supports edition 2024.
- `forAllSystems` is `lib.genAttrs` over the four common systems; flake-utils not needed.
- Checks: `cargoCheckHook` runs `cargo test`: unit tests in the lib, and the `tests/` integration suites.
- `git` is the only extra check input. The fake `claude` scripts take their shebang from `bash` on `PATH` (stdenv provides it), since the sandbox has no `/usr/bin/env`.
- `meta` and `versionCheckHook` match the Nixpkgs package (`pkgs/by-name/ai/ai`), so the flake build checks what Nixpkgs checks.
