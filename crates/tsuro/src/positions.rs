//! Posição de leitura por arquivo (`página, zoom` + identidade).
//!
//! Mesmo padrão de `recents_file()` em `browse.rs` e `prefs_file()` em
//! `prefs.rs`: `TSURO_POSITIONS` vence, depois macOS
//! `~/Library/Application Support/Tsuro/positions`, `$XDG_DATA_HOME`,
//! `~/.local/share`, por fim o temp dir. Leitura cega: qualquer erro → vazio.
//!
//! Formato (uma entrada por linha, mais recente primeiro, máx 50):
//! `página\tzoom\ttamanho\tmtime\tmodo\tcaminho` com `\`/`\t`/`\n` escapados
//! no caminho. Linha no formato antigo (sem o campo `modo`) é lida como
//! `Single`; linha malformada é ignorada, nunca derruba a leitura.

use std::path::{Path, PathBuf};

use crate::session::{ViewMode, Zoom, ZoomFactor};

/// Entradas guardadas por arquivo (cap LRU).
pub const MAX_POSITIONS: usize = 50;

#[derive(Debug, Clone, Copy)]
pub struct DocPosition {
    /// Índice 0-based da última página lida.
    pub page: u32,
    pub zoom: Zoom,
    /// Modo de página da última leitura (única/contínua).
    pub mode: ViewMode,
    /// Identidade do arquivo (invalida se o documento mudou).
    pub size: u64,
    pub mtime: u64,
}

#[cfg(test)]
use std::cell::RefCell;

#[cfg(test)]
thread_local! {
    static POSITIONS_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub fn with_positions_path<R>(path: PathBuf, f: impl FnOnce() -> R) -> R {
    POSITIONS_PATH.with(|slot| *slot.borrow_mut() = Some(path));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    POSITIONS_PATH.with(|slot| *slot.borrow_mut() = None);
    match result {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

pub fn positions_file() -> PathBuf {
    #[cfg(test)]
    {
        if let Some(p) = POSITIONS_PATH.with(|slot| slot.borrow().clone()) {
            return p;
        }
    }
    if let Some(p) = std::env::var_os("TSURO_POSITIONS") {
        return PathBuf::from(p);
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join("Library/Application Support/Tsuro/positions");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("tsuro/positions");
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        return PathBuf::from(home).join(".local/share/tsuro/positions");
    }
    std::env::temp_dir().join("tsuro-positions")
}

/// Tamanho + mtime (segundos) do arquivo; `None` se ilegível.
pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((meta.len(), mtime))
}

fn escape_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unescape_path(raw: &str) -> PathBuf {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    PathBuf::from(out)
}

fn encode_zoom(zoom: Zoom) -> String {
    match zoom {
        Zoom::Width => "width".to_string(),
        Zoom::Page => "page".to_string(),
        Zoom::Manual(z) => format!("manual:{}", z.get()),
    }
}

fn encode_mode(mode: ViewMode) -> &'static str {
    match mode {
        ViewMode::Single => "single",
        ViewMode::Continuous => "continuous",
    }
}

fn decode_mode(raw: &str) -> ViewMode {
    match raw {
        "continuous" => ViewMode::Continuous,
        _ => ViewMode::Single,
    }
}

fn decode_zoom(raw: &str) -> Zoom {
    match raw {
        "page" => Zoom::Page,
        _ if raw.starts_with("manual:") => raw["manual:".len()..]
            .parse::<f32>()
            .ok()
            .filter(|f| f.is_finite())
            .map(|f| Zoom::Manual(ZoomFactor::new(f)))
            .unwrap_or(Zoom::Width),
        _ => Zoom::Width,
    }
}

fn encode_entry(path: &Path, pos: &DocPosition) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        pos.page,
        encode_zoom(pos.zoom),
        pos.size,
        pos.mtime,
        encode_mode(pos.mode),
        escape_path(path),
    )
}

fn decode_entry(line: &str) -> Option<(PathBuf, DocPosition)> {
    // 6 campos: página, zoom, tamanho, mtime, modo, caminho.
    // 5 campos: formato anterior ao modo persistido (lido como `Single`).
    let fields: Vec<&str> = line.split('\t').collect();
    let (page, zoom, size, mtime, mode, path) = match fields[..] {
        [page, zoom, size, mtime, mode, path] => (page, zoom, size, mtime, decode_mode(mode), path),
        [page, zoom, size, mtime, path] => (page, zoom, size, mtime, ViewMode::Single, path),
        _ => return None,
    };
    let page = page.parse::<u32>().ok()?;
    let zoom = decode_zoom(zoom);
    let size = size.parse::<u64>().ok()?;
    let mtime = mtime.parse::<u64>().ok()?;
    let path = unescape_path(path);
    if path.as_os_str().is_empty() {
        return None;
    }
    Some((
        path,
        DocPosition {
            page,
            zoom,
            mode,
            size,
            mtime,
        },
    ))
}

/// Lê todas as entradas (mais recente primeiro); erro ou lixo → ignora.
pub fn read_positions() -> Vec<(PathBuf, DocPosition)> {
    let Ok(raw) = std::fs::read_to_string(positions_file()) else {
        return Vec::new();
    };
    raw.lines().filter_map(decode_entry).collect()
}

pub fn save_positions(entries: &[(PathBuf, DocPosition)]) -> std::io::Result<()> {
    let file = positions_file();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = String::new();
    for (path, pos) in entries.iter().take(MAX_POSITIONS) {
        body.push_str(&encode_entry(path, pos));
        body.push('\n');
    }
    std::fs::write(file, body)
}

/// Pura e testável: move `path` para o topo (dedup), limita em 50.
pub fn record_position(
    mut entries: Vec<(PathBuf, DocPosition)>,
    path: PathBuf,
    pos: DocPosition,
) -> Vec<(PathBuf, DocPosition)> {
    entries.retain(|(p, _)| p != &path);
    entries.insert(0, (path, pos));
    entries.truncate(MAX_POSITIONS);
    entries
}

/// Posição válida para `path`: entrada existe e identidade confere.
pub fn find_position(entries: &[(PathBuf, DocPosition)], path: &Path) -> Option<DocPosition> {
    let (size, mtime) = file_identity(path)?;
    entries
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, pos)| *pos)
        .filter(|pos| pos.size == size && pos.mtime == mtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_positions(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tsuro-positions-unit-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ))
    }

    fn pos(page: u32) -> DocPosition {
        DocPosition {
            page,
            zoom: Zoom::Width,
            mode: ViewMode::Single,
            size: 10,
            mtime: 20,
        }
    }

    #[test]
    fn missing_or_garbage_reads_empty() {
        let path = temp_positions("missing");
        let _ = std::fs::remove_file(&path);
        with_positions_path(path.clone(), || {
            assert!(read_positions().is_empty());
            std::fs::write(&path, "lixo\n1\twidth\n").unwrap();
            assert!(read_positions().is_empty());
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn roundtrip_and_lru_cap() {
        let path = temp_positions("roundtrip");
        let _ = std::fs::remove_file(&path);
        with_positions_path(path.clone(), || {
            let mut entries = Vec::new();
            for i in 0..(MAX_POSITIONS as u32 + 5) {
                entries = record_position(entries, PathBuf::from(format!("/doc/{i}.pdf")), pos(i));
            }
            assert_eq!(entries.len(), MAX_POSITIONS);
            assert_eq!(
                entries[0].0,
                PathBuf::from(format!("/doc/{}.pdf", MAX_POSITIONS + 4))
            );
            // Re-gravar move para o topo sem duplicar.
            entries = record_position(entries, PathBuf::from("/doc/0.pdf"), pos(99));
            assert_eq!(entries.len(), MAX_POSITIONS);
            assert_eq!(entries[0].1.page, 99);
            save_positions(&entries).unwrap();
            let back = read_positions();
            assert_eq!(back.len(), MAX_POSITIONS);
            assert_eq!(back[0].1.page, 99);
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn zoom_path_and_mode_roundtrip() {
        let path = temp_positions("escape");
        let _ = std::fs::remove_file(&path);
        with_positions_path(path.clone(), || {
            let tricky = PathBuf::from("/tmp/a\tb\\c.pdf");
            let entries = record_position(
                Vec::new(),
                tricky.clone(),
                DocPosition {
                    page: 3,
                    zoom: Zoom::Manual(ZoomFactor::new(1.5)),
                    mode: ViewMode::Continuous,
                    size: 7,
                    mtime: 8,
                },
            );
            save_positions(&entries).unwrap();
            let back = read_positions();
            assert_eq!(back.len(), 1);
            assert_eq!(back[0].0, tricky);
            assert_eq!(back[0].1.page, 3);
            assert!(matches!(back[0].1.zoom, Zoom::Manual(_)));
            assert_eq!(back[0].1.mode, ViewMode::Continuous);
        });
        let _ = std::fs::remove_file(&path);
    }

    /// Arquivo escrito antes do campo `modo` continua legível (modo = Single).
    #[test]
    fn legacy_five_field_line_reads_as_single() {
        let path = temp_positions("legacy");
        let _ = std::fs::remove_file(&path);
        with_positions_path(path.clone(), || {
            std::fs::write(&path, "12\tpage\t99\t7\t/tmp/velho.pdf\n").unwrap();
            let back = read_positions();
            assert_eq!(back.len(), 1);
            assert_eq!(back[0].0, PathBuf::from("/tmp/velho.pdf"));
            assert_eq!(back[0].1.page, 12);
            assert!(matches!(back[0].1.zoom, Zoom::Page));
            assert_eq!(back[0].1.mode, ViewMode::Single);
            assert_eq!(back[0].1.size, 99);
            assert_eq!(back[0].1.mtime, 7);
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stale_identity_is_rejected() {
        let dir = std::env::temp_dir();
        let pdf = dir.join(format!(
            "tsuro-positions-unit-{}-stale.pdf",
            std::process::id()
        ));
        std::fs::write(&pdf, b"0123456789").unwrap();
        let (size, mtime) = file_identity(&pdf).unwrap();
        let entries = vec![(
            pdf.clone(),
            DocPosition {
                page: 5,
                zoom: Zoom::Page,
                mode: ViewMode::Continuous,
                size,
                mtime,
            },
        )];
        assert_eq!(find_position(&entries, &pdf).map(|p| p.page), Some(5));
        std::fs::write(&pdf, b"0123456789abcdef").unwrap();
        assert!(find_position(&entries, &pdf).is_none());
        let _ = std::fs::remove_file(&pdf);
    }
}
