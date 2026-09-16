<p align="right">🇧🇷 <a href="CONTRIBUTING.pt-BR.md">Português</a></p>

# Contributing to TsuroPDF

Issues, PRs, and commits in **English or PT-BR** — both are first-class. Keep PRs small and one-topic.

## Mission gate

TsuroPDF is a fast, lightweight PDF reader: read, mark, print. Nothing else. Before proposing anything, ask: does it make reading, marking, or printing faster, lighter, or clearer? Out of scope, on purpose: forms, cloud, collaboration, suites. Proposals outside the mission will be declined or deferred.

## Issues

Check open issues first — duplicates get closed. A good report has:

- TsuroPDF version (release tag, e.g. `v0.2.0`, or commit hash)
- OS (macOS Apple Silicon, Windows x64)
- Steps to reproduce, expected vs. actual behavior
- A sample PDF: attach one, or name a file in `public/samples/`
- For signature reports: the state shown in the signatures panel

A new issue gets the `needs-triage` label from `.github/workflows/issues.yml`. A very short body also gets `needs-info` and a comment asking for the fields above.

## Pull requests

1. Branch from `main` (`feat/<topic>`, `fix/<topic>`).
2. Reproduce bugs with a fixture PDF first (`public/samples/`).
3. Run the tests before opening the PR:

   ```bash
   cargo test -p tsuro
   cargo test -p tsuro-sign   # only if you touched signatures
   ```

   `.github/workflows/pr.yml` runs the same commands on macos-14 and windows-latest.
   `.github/workflows/review.yml` runs rustfmt --check and clippy on those same packages.
4. New viewer code → `crates/tsuro/...`; signature code → `crates/tsuro-sign/...`.

Commits follow [Conventional Commits v1.0.0](https://www.conventionalcommits.org/en/v1.0.0/): `type(scope): description (#issue) (#PR)`.

```
feat(view): continuous scroll page mode (#26) (#36)
fix(tsuro): keep old bitmap when switching zoom (#21)
```

Types: `feat`, `fix`, plus `docs`, `chore`, `assets`, `ci` and the other spec-recommended types. Scopes in this repo: `view`, `nav`, `print`, `tsuro`, `sign`, `ci`, `mac`. Breaking changes use `!` (`feat(view)!: ...`) with a `BREAKING CHANGE:` footer.

## Don't

- Touch the legacy tree (`src/`, `src-tauri/`) — reference only.
- Hand-edit fixtures in `public/samples/` — regenerate via `scripts/generate_samples.py`.
- Add dependencies without asking in the issue/PR first.
- Commit secrets or keys. Never `git push --force`.
