<p align="right">🇧🇷 <a href="README.pt-BR.md">Português</a></p>

<p align="center">
  <img src="docs/hero.png" alt="TsuroPDF hero — origami crane over misty mountains" width="100%">
</p>

<h1 align="center">TsuroPDF</h1>

<p align="center">
  <a href="https://github.com/claudioorjunior/tsuro-pdf/releases"><img src="https://img.shields.io/github/v/release/claudioorjunior/tsuro-pdf?sort=semver&display_name=release" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/claudioorjunior/tsuro-pdf" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/macOS-Apple_Silicon-000?logo=apple&logoColor=white" alt="macOS Apple Silicon">
  <img src="https://img.shields.io/badge/Windows-x64-0078D4?logo=windows&logoColor=white" alt="Windows x64">
  <img src="https://img.shields.io/badge/built_with-Rust-CE422B?logo=rust&logoColor=white" alt="Built with Rust">
</p>

<p align="center"><strong>A fast, lightweight, open-source PDF reader for the desktop. Read, mark, print. Nothing else.</strong></p>

TsuroPDF is a native viewer written in Rust ([iced](https://iced.rs/) + [Pdfium](https://pdfium.googlesource.com/pdfium/)). It targets any PDF: large files, demanding typography, digitally signed documents. If a feature does not make reading, marking, or printing faster, lighter, or clearer, it does not belong here. No forms, no cloud, no collaboration, no suite.

## Features

- **Open** from the toolbar, by drag and drop, or from the empty-state browser (folders and recent files)
- **Pages** — thumbnail panel with document outline tab, previous/next, page N / total counter, go-to-page with keyboard navigation
- **View modes** — single page or continuous scroll
- **Resume reading** — reopens each file where you left off (page + zoom); back/forward history
- **Zoom** — fit to width, fit to page, `+` / `-`
- **Search** the extracted text, with match count
- **Signatures** — on-demand panel with signer and cryptographic status
- **Copy** selected text, when there is a selection
- **Print** — built-in dialog with direct spool (renders a 200 DPI print PDF, no native dialog)
- **View rotation** — 90° per session, plus dark/light theme

Annotations and highlights are part of the mission but not in the viewer yet. What stays out, on purpose, is everything else.

## Install

The short way, with [Bun](https://bun.sh/):

```bash
bun run install:tsuro -- --install
```

It queries the latest [GitHub Release](https://github.com/claudioorjunior/tsuro-pdf/releases), downloads the artifact for your OS, and verifies the SHA-256. Without `--install` it only downloads. `--check` reports whether a newer version exists; `--version v0.2.0` pins a tag. If there is no release for your OS yet, the script prints the clone path and exits with code 1.

Or grab the file from the release page:

- **Mac Apple Silicon** — `TsuroPDF-{version}-aarch64-apple-darwin.dmg`. Open the DMG and drag TsuroPDF to Applications.
- **Windows x64** — `TsuroPDF-{version}-x86_64-pc-windows-msvc-setup.exe`. Installs to `%LOCALAPPDATA%\Programs\TsuroPDF`, no admin needed. `/S` is the silent install.

No builds for Intel Macs or Linux yet.

**First launch on macOS.** Signing is ad-hoc (no Apple Developer Program). Gatekeeper will warn: right-click TsuroPDF → Open.

**First launch on Windows.** If SmartScreen appears: More info → Run anyway.

## Build from source

```bash
git clone https://github.com/claudioorjunior/tsuro-pdf.git
cd tsuro-pdf
./scripts/bundle-macos.sh        # macOS .app in dist/
```

```powershell
powershell -File scripts/bundle-windows.ps1   # Windows NSIS installer
```

Developing the viewer needs stable [Rust](https://rustup.rs/) (`rustup default stable`) plus the [Pdfium](https://github.com/bblanchon/pdfium-binaries) library in the project dir or on the system — `bundle-macos.sh` handles that on macOS. Node.js is only needed for the legacy viewer (Tauri + PDF.js), which is not the product.

Run from code:

```bash
cargo test -p tsuro-sign
cargo run -p tsuro -- public/samples/guia-folio.pdf
```

With no argument the window opens empty. Controls live in the toolbar, not in shortcuts.

## Samples

Three PDFs in `public/samples/`:

| File | What it exercises |
| --- | --- |
| `guia-folio.pdf` | Typography, tables, accents, search |
| `sumario-folio.pdf` | Bookmarks (outline) driving the Sumário tab |
| `contrato-assinado.pdf` | `/Sig` field with a demo self-signed certificate |

The contract certificate is **self-signed**. TsuroPDF treats it as cryptographically intact but with no public trust chain. The expected state is "intact (no public trust)".

## Digital signatures

Detection and verification live in the Rust crate `tsuro-sign`:

- walks AcroForm and `/Sig` dictionaries
- reads PKCS#7/CMS (`adbe.pkcs7.detached` profile)
- checks `ByteRange` and `messageDigest`
- verifies RSA + SHA-256 over the signed attributes

Panel states: valid, intact (no public trust), document modified, invalid, unsupported, certificate expired or not yet valid.

## Project structure

```
crates/tsuro            native viewer (iced + Pdfium) — the product
crates/tsuro-sign       signature engine (PDF + CMS)
public/samples          sample PDFs
docs/hero.png           README hero
src-tauri, src          legacy viewer (Tauri + React + PDF.js)
```

The legacy tree stays in the repo for reference. It is not what ships as TsuroPDF.

```bash
npm install
npm run test:sign
npm run dev          # legacy browser viewer (http://127.0.0.1:43177)
npm run tauri dev
```

## Contributing

Read, verify, and packaging contributions are welcome. TsuroPDF intends to stay small: every proposal is judged against the mission gate (faster, lighter, or clearer reading, marking, or printing). Issues, PRs, and commits in English or PT-BR; keep PRs small and one-topic. See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide.

## License

[MIT](LICENSE). Copyright (c) 2026 Tsuro contributors.
