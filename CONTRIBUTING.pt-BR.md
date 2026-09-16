<p align="right">🇺🇸 <a href="CONTRIBUTING.md">English</a></p>

# Contribuindo com o TsuroPDF

Issues, PRs e commits em **inglês ou PT-BR** — os dois valem igual. PRs pequenos, um assunto por vez.

## Missão

TsuroPDF é um leitor de PDF rápido e leve: ler, marcar, imprimir. Nada além disso. Antes de propor qualquer coisa, pergunte: isso deixa ler, marcar ou imprimir mais rápido, mais leve ou mais claro? Fora do escopo, de propósito: formulários, nuvem, colaboração, suítes. Proposta fora da missão é recusada ou adiada.

## Issues

Veja as issues abertas antes — duplicada é fechada. Um bom relato tem:

- Versão do TsuroPDF (tag da release, ex. `v0.2.0`, ou hash do commit)
- Sistema (macOS Apple Silicon, Windows x64)
- Passos para reproduzir, comportamento esperado vs. obtido
- Um PDF de exemplo: anexe um, ou indique um arquivo em `public/samples/`
- Para relatos de assinatura: o estado mostrado no painel de assinaturas

Uma issue nova recebe a label `needs-triage` de `.github/workflows/issues.yml`. Um corpo muito curto também recebe `needs-info` e um comentário pedindo os campos acima.

## Pull requests

1. Crie a branch a partir da `main` (`feat/<tema>`, `fix/<tema>`).
2. Reproduza bugs com um PDF de exemplo antes (`public/samples/`).
3. Rode os testes antes de abrir o PR:

   ```bash
   cargo test -p tsuro
   cargo test -p tsuro-sign   # só se mexeu em assinaturas
   ```

   `.github/workflows/pr.yml` roda os mesmos comandos em macos-14 e windows-latest.
   `.github/workflows/review.yml` roda rustfmt --check e clippy nesses mesmos pacotes.
4. Código novo do visor → `crates/tsuro/...`; de assinaturas → `crates/tsuro-sign/...`.

Commits seguem [Conventional Commits v1.0.0](https://www.conventionalcommits.org/en/v1.0.0/): `tipo(escopo): descrição (#issue) (#PR)`.

```
feat(view): rolagem contínua por modo de página (#26) (#36)
fix(tsuro): mantém bitmap antigo ao trocar o zoom (#21)
```

Tipos: `feat`, `fix`, mais `docs`, `chore`, `assets`, `ci` e os outros tipos recomendados pela spec. Escopos neste repo: `view`, `nav`, `print`, `tsuro`, `sign`, `ci`, `mac`. Breaking changes usam `!` (`feat(view)!: ...`) com rodapé `BREAKING CHANGE:`.

## Não

- Mexer no legado (`src/`, `src-tauri/`) — só consulta.
- Editar fixtures em `public/samples/` à mão — regenere com `scripts/generate_samples.py`.
- Adicionar dependências sem perguntar na issue/PR antes.
- Commitar segredos ou chaves. Nunca `git push --force`.
