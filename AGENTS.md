# AGENTS.md — Tsuro PDF

Source of truth for agent instructions in this repo. Do not duplicate in `CLAUDE.md` or Cursor rules — update this file instead.

## Mission gate

Tsuro PDF é um leitor de PDF veloz, otimizado e leve: ler, anotar, marcar, imprimir. Nada além disso.

Antes de propor ou implementar uma mudança, pergunte: isso deixa ler, anotar, marcar ou imprimir mais rápido, mais leve ou mais claro? Recurso fora disso — suíte, nuvem, formulário, colaboração — recuse ou adie.

## Blast radius first

- `src/`, `src-tauri/` are legacy (Tauri/React). NEVER extend. Bugfix only with explicit permission.
- NEVER hand-roll crypto. Signatures live in `crates/tsuro-sign` (CMS engine over `rsa`/`x509-parser`); use it, don't reimplement.
- Fixtures in `public/samples/` (`guia-folio.pdf`, `contrato-assinado.pdf`) are read-only test inputs. NEVER overwrite — regenerate via `scripts/generate_samples.py`.

## Stack

Rust edition 2021, stable toolchain (`rust-toolchain.toml`). Viewer: `iced 0.13` + `pdfium-render 0.8` (needs `libpdfium.dylib` at runtime — `PDFIUM_MISSING` in `engine.rs`).

## Commands (exact)

- `cargo test -p tsuro` — session, browse, engine (run before every PR)
- `cargo test -p tsuro-sign` — digital signatures
- `.github/workflows/pr.yml` — same two commands on `pull_request` (macos-14, windows-latest)
- `.github/workflows/review.yml` — rustfmt --check and clippy on `pull_request` (macos-14, tsuro + tsuro-sign only)
- `.github/workflows/issues.yml` — `needs-triage` / `needs-info` on issue open and edit
- `cargo run -p tsuro -- public/samples/guia-folio.pdf` — manual check
- `./scripts/bundle-macos.sh` — macOS `.app`
- `powershell -File scripts/bundle-windows.ps1` — NSIS CurrentUser
- `bun run install:tsuro` — instalador provisório (GitHub Releases)
- `bun run test:install` — plano de install (fixtures, sem rede)

## Code map (where new code goes)

- `crates/tsuro/src/session.rs` — session state (`Session`, `Message`, `OpenSource`, `Theme`); nothing visual here
- `crates/tsuro/src/view.rs` — chrome only; no state
- `crates/tsuro/src/browse.rs` — empty-state folders, recents, `EmptyState.theme` (`EmptyState`, `FsEntry`)
- `crates/tsuro/src/page.rs` — public PDF types (`PageNo`, `MediaBox`, `Bitmap`, `TextLayer`)
- `crates/tsuro/src/engine.rs` — `pub(crate)` Pdfium worker (thread + channel); stays private to the crate
- `crates/tsuro/src/print.rs` — `pub(crate)` seleção + PDF de impressão (`PrintRange`, `PrintSelection`, `print_selection_pdf`)
- `crates/tsuro/src/spool.rs` — `pub(crate)` fronteira com o crate `printers` (`PrinterInfo`, `list_printers`, `spool_pdf`)
- `crates/tsuro/src/kiri.rs` — Kiri tokens + chrome styles (`Theme`, `Tokens`, `status_dot_color`); icons Ori in `assets/icons/ori/`
- `crates/tsuro/src/prefs.rs` — `theme=dark|light` prefs file (mirrors `recents_file()` pattern)
- `crates/tsuro-sign/` — PDF + CMS signature engine (`SigError`, `CertificateInfo`)
- `scripts/install-tsuro.ts` — instalador Bun (não entra no `.app`)
- `.github/workflows/pr.yml` — `cargo test` on pull_request
- `.github/workflows/review.yml` — rustfmt --check and clippy on pull_request
- `.github/workflows/issues.yml` — triage labels on issues
- `.github/workflows/release.yml` — DMG + NSIS no tag `v*`
- New viewer code → `crates/tsuro/...`; new signature code → `crates/tsuro-sign/...`. NEVER in the legacy tree.

## Style (example beats prose)

- `use`/`pub` via `crate::` paths; engine internals stay `pub(crate)`:

  ```rust
  // ✅ Good
  use crate::page::PageNo;
  use crate::session::{Message, Session};
  pub(crate) mod engine;

  // ❌ Bad
  use super::page::PageNo;      // no super:: imports
  pub mod engine;               // engine is crate-internal, not public API
  ```

- `PageNo` is 0-based (`PageNo::first()` = index 0). Don't invent 1-based page math.
- Chrome in `view.rs`; state in `session.rs`. Small diffs; no new deps without asking.

## Boundaries

- ✅ Always: run `cargo test -p tsuro` before opening a PR; reproduce bugs with a fixture PDF first; release notes citam PRs mergeadas com @autores (automático via `generate-notes` no `release.yml`, highlights manuais por versão).
- ✅ Commits: [Conventional Commits v1.0.0](https://www.conventionalcommits.org/en/v1.0.0/) — `type(scope): description (#issue) (#PR)`, EN or PT-BR; `!` + `BREAKING CHANGE:` footer for breaking changes.
- ⚠️ Ask first: new dependencies, changes under `crates/tsuro-sign/`, touching the legacy tree.
- 🚫 Never: commit secrets/keys, modify fixtures by hand, `git push --force`.

## Verify, don't assume

Before referencing any function, read its defining file in this session. Citations without preceding reads are drafts to verify.

## Mistake journal

- 2026-09-08: edit ranges guessed from stale line numbers ate struct fields (`Ready.selection`, enum brace) → re-read the exact region before every edit; one op per patch per file unless bodies are exact snapshot lines.
