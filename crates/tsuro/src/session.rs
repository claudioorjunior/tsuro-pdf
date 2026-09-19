use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use iced::clipboard;
use iced::event::{self, Event};
use iced::keyboard::{self, key::Named, Key};
use iced::widget::{image, scrollable, text_editor};
use iced::window;
use iced::Task;
use tsuro_sign::{analyze_pdf, PdfAnalysis};

use crate::browse::{
    display_path, drop_recent, is_pdf, list_path, load_recents, merge_recents, parent_of,
    push_recent, read_recents, save_recents, EmptyState, FsEntry,
};
use crate::engine::PdfiumEngine;
use crate::kiri::Theme;
use crate::page::{
    EngineError, Glyph, MediaBox, Outline, OutlineItem, PageEngine, PageNo, PageSurface, Quad,
    Scale, TextLayer, Viewport,
};
use crate::positions::{
    file_identity, find_position, read_positions, record_position, save_positions, DocPosition,
};
use crate::prefs::{read_theme, save_theme};
use crate::print::{
    print_selection_pdf, write_and_open_print_pdf, PrintOrientation, PrintRange, PrintSelection,
    MAX_COPIES,
};
use crate::spool::{list_printers, spool_pdf, PrinterInfo};

pub(crate) const THUMB_WIDTH: f32 = 120.0;
pub(crate) const THUMB_ROW: f32 = 156.0;
const THUMB_VISIBLE: u32 = 8;
const THUMB_PREFETCH: u32 = 3;

/// Passo vertical de uma linha da árvore do sumário, usado só para manter a
/// linha do cursor visível ao andar com ↑/↓. `outline_tab` em view.rs mede
/// texto 12 com line-height 1.3 (=15,6) + padding [9,10] (=18) + spacing 8
/// = 41,6; o valor é arredondado para baixo de propósito: subestimar deixa a
/// linha um pouco abaixo do topo (visível), superestimar a esconderia.
pub(crate) const OUTLINE_ROW: f32 = 41.0;

/// RGBA byte budget for neighbor/speculative page surfaces (`visible ± 1`).
/// The mandatory visible-page bitmap at its current scale is excluded.
const NEIGHBOR_CACHE_BUDGET: usize = 64 * 1024 * 1024;

/// Larguras do chrome (espelham `view.rs`); base da geometria do contínuo.
pub(crate) const PAGES_PANEL_W: f32 = 156.0;
pub(crate) const SIG_PANEL_W: f32 = 220.0;
const PANES_GAP: f32 = 12.0;
const CHROME_PAD: f32 = 8.0;
/// Célula do contínuo replica o padding de `page_pane` (view.rs).
pub(crate) const DOC_PAD_TOP: f32 = 28.0;
pub(crate) const DOC_PAD_BOTTOM: f32 = 56.0;
pub(crate) const DOC_PAD_X: f32 = 24.0;
pub(crate) const DOC_GAP: f32 = 16.0;

/// Intervalo do tique da tela de abertura (issue #41): 12 passos dos 90 ms
/// fecham o ciclo de ~1,1 s da barra indeterminada (`view::LOADING_STEPS`).
const LOADING_TICK: std::time::Duration = std::time::Duration::from_millis(90);

#[derive(Debug, Clone)]
pub enum OpenSource {
    Path(PathBuf),
    Dropped(PathBuf),
}

impl OpenSource {
    pub fn path(&self) -> &std::path::Path {
        match self {
            OpenSource::Path(p) | OpenSource::Dropped(p) => p,
        }
    }

    pub fn from_dialog() -> Option<Self> {
        rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_file()
            .map(OpenSource::Path)
    }
}

/// Nome sugerido da cópia marcada: `contrato.pdf` → `contrato (marcado).pdf`.
/// Sem extensão o sufixo é o mesmo; a última extensão é a única trocada
/// (`ata.v1.tar.gz` → `ata.v1.tar (marcado).pdf`).
pub fn suggested_marked_name(source: &std::path::Path) -> String {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "documento".into());
    format!("{stem} (marcado).pdf")
}

#[derive(Debug, Clone, Copy)]
pub struct ZoomFactor(f32);

impl ZoomFactor {
    pub fn new(raw: f32) -> Self {
        ZoomFactor(raw.clamp(0.25, 8.0))
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Zoom {
    Width,
    Page,
    Manual(ZoomFactor),
}

impl Zoom {
    pub fn scale(self, viewport: Viewport, media: MediaBox) -> Scale {
        let width = media.width.max(1.0);
        let height = media.height.max(1.0);
        let vw = viewport.width.max(1.0);
        let vh = viewport.height.max(1.0);
        let factor = match self {
            Zoom::Width => vw / width,
            Zoom::Page => (vw / width).min(vh / height),
            Zoom::Manual(z) => z.get(),
        };
        Scale::from_factor(factor)
    }
}

/// Modo de página (issue #26; Espelhadas fora do MVP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Single,
    Continuous,
}

#[derive(Debug, Clone)]
pub struct Search {
    query: String,
    hits: Vec<Hit>,
}

impl Search {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    fn derive(query: &str, pages: &[Option<TextLayer>]) -> Self {
        if query.is_empty() {
            return Search {
                query: query.to_string(),
                hits: Vec::new(),
            };
        }
        let mut hits = Vec::new();
        for layer in pages.iter().flatten() {
            hits.extend(find_hits(query, layer));
        }
        hits.sort_by_key(|hit| (hit.page.index(), hit.range.start));
        Search {
            query: query.to_string(),
            hits,
        }
    }

    fn extend_page(&mut self, layer: &TextLayer) {
        if self.query.is_empty() {
            return;
        }
        let page_idx = layer.page.index();
        self.hits.retain(|hit| hit.page != layer.page);
        let mut page_hits = find_hits(&self.query, layer);
        page_hits.sort_by_key(|hit| hit.range.start);
        let pos = self
            .hits
            .partition_point(|hit| (hit.page.index(), hit.range.start) < (page_idx, 0));
        self.hits.splice(pos..pos, page_hits);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LowerChar {
    lower: char,
    byte_start: usize,
    byte_end: usize,
}

fn lower_plain_map(plain: &str) -> Vec<LowerChar> {
    let mut mapped = Vec::new();
    for (byte_start, ch) in plain.char_indices() {
        let byte_end = byte_start + ch.len_utf8();
        for lower in ch.to_lowercase() {
            mapped.push(LowerChar {
                lower,
                byte_start,
                byte_end,
            });
        }
    }
    mapped
}

fn case_insensitive_byte_ranges(plain: &str, needle: &str) -> Vec<(usize, usize)> {
    let needle_chars: Vec<char> = needle.chars().flat_map(|ch| ch.to_lowercase()).collect();
    if needle_chars.is_empty() {
        return Vec::new();
    }
    let mapped = lower_plain_map(plain);
    if mapped.len() < needle_chars.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + needle_chars.len() <= mapped.len() {
        if mapped[i..i + needle_chars.len()]
            .iter()
            .zip(&needle_chars)
            .all(|(entry, &nc)| entry.lower == nc)
        {
            let start = mapped[i].byte_start;
            let end = mapped[i + needle_chars.len() - 1].byte_end;
            out.push((start, end));
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

fn find_hits(query: &str, layer: &TextLayer) -> Vec<Hit> {
    let ranges = case_insensitive_byte_ranges(&layer.plain, query);
    let mut hits = Vec::with_capacity(ranges.len());
    let mut glyph_idx = 0usize;
    let mut byte_cursor = 0usize;
    for (start, end) in ranges {
        let quad =
            quads_for_range_monotonic(&layer.glyphs, start, end, &mut glyph_idx, &mut byte_cursor);
        hits.push(Hit {
            page: layer.page,
            range: TextRange { start, end },
            quad,
        });
    }
    hits
}

fn quads_for_range_monotonic(
    glyphs: &[Glyph],
    start: usize,
    end: usize,
    glyph_idx: &mut usize,
    byte_cursor: &mut usize,
) -> Quad {
    while *glyph_idx < glyphs.len() {
        let next = *byte_cursor + glyphs[*glyph_idx].cluster.len();
        if next > start {
            break;
        }
        *byte_cursor = next;
        *glyph_idx += 1;
    }
    let mut acc: Option<Quad> = None;
    let mut cursor = *byte_cursor;
    for glyph in &glyphs[*glyph_idx..] {
        let next = cursor + glyph.cluster.len();
        if cursor < end && next > start {
            acc = Some(match acc {
                None => glyph.quad,
                Some(q) => q.union(glyph.quad),
            });
        }
        cursor = next;
        if cursor >= end {
            break;
        }
    }
    acc.unwrap_or(Quad::from_rect(0.0, 0.0, 0.0, 0.0))
}

/// Quad de um range arbitrário (mesma origem dos hits de busca).
pub(crate) fn quad_for_range(glyphs: &[Glyph], start: usize, end: usize) -> Quad {
    let mut glyph_idx = 0usize;
    let mut byte_cursor = 0usize;
    quads_for_range_monotonic(glyphs, start, end, &mut glyph_idx, &mut byte_cursor)
}

/// Um retângulo por linha do range (padrão dos leitores: nunca pintar o vão
/// entre linhas). Agrupa glifos vizinhos com sobreposição vertical; x que
/// volta para trás abre nova linha (colunas). Texto não-horizontal degrada
/// para um grupo só (equivale ao union antigo).
/// ponytail: heurística LTR por bbox; segmentação por baseline se precisar.
pub(crate) fn quads_for_range_by_line(glyphs: &[Glyph], start: usize, end: usize) -> Vec<Quad> {
    let mut cursor = 0usize;
    let mut lines: Vec<(f32, f32, f32, f32)> = Vec::new();
    for glyph in glyphs {
        let next = cursor + glyph.cluster.len();
        if cursor < end && next > start {
            let (x0, y0, x1, y1) = quad_bbox(glyph.quad);
            let merge = match lines.last() {
                Some(last) => y0 <= last.3 && y1 >= last.1 && x0 >= last.0,
                None => false,
            };
            if merge {
                let last = lines.last_mut().expect("checked above");
                last.0 = last.0.min(x0);
                last.1 = last.1.min(y0);
                last.2 = last.2.max(x1);
                last.3 = last.3.max(y1);
            } else {
                lines.push((x0, y0, x1, y1));
            }
        }
        cursor = next;
        if cursor >= end {
            break;
        }
    }
    lines
        .into_iter()
        .map(|(x0, y0, x1, y1)| Quad::from_rect(x0, y0, x1, y1))
        .collect()
}

fn quad_bbox(quad: Quad) -> (f32, f32, f32, f32) {
    let xs = [quad.x0, quad.x1, quad.x2, quad.x3];
    let ys = [quad.y0, quad.y1, quad.y2, quad.y3];
    (
        xs.iter().fold(f32::INFINITY, |a, &b| a.min(b)),
        ys.iter().fold(f32::INFINITY, |a, &b| a.min(b)),
        xs.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b)),
        ys.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b)),
    )
}

/// Retângulo exibido `[x, y, w, h]` (px CSS, Y para baixo) de um quad da
/// mídia original (espaço PDF, Y para cima), desfazendo a rotação da vista
/// (`rotation` em quartos horários, como vista na tela).
pub(crate) fn display_rect(
    quad: Quad,
    media: MediaBox,
    rotation: u8,
    dw: f32,
    dh: f32,
) -> [f32; 4] {
    let (x0, y0, x1, y1) = quad_bbox(quad);
    let w = media.width.max(1.0);
    let h = media.height.max(1.0);
    let (rx0, ry0, rx1, ry1) = match rotation & 3 {
        0 => (x0, h - y1, x1, h - y0),
        1 => (y0, x0, y1, x1),
        2 => (w - x1, y0, w - x0, y1),
        _ => (h - y1, w - x1, h - y0, w - x0),
    };
    let rw = if rotation & 1 == 1 { h } else { w };
    let rh = if rotation & 1 == 1 { w } else { h };
    [
        rx0 / rw * dw,
        ry0 / rh * dh,
        (rx1 - rx0) / rw * dw,
        (ry1 - ry0) / rh * dh,
    ]
}

/// Inverso: ponto exibido (px CSS, Y para baixo) → ponto da mídia original
/// (espaço PDF, Y para cima).
pub(crate) fn page_pt_at(
    point: [f32; 2],
    media: MediaBox,
    rotation: u8,
    dw: f32,
    dh: f32,
) -> [f32; 2] {
    let w = media.width.max(1.0);
    let h = media.height.max(1.0);
    let (rw, rh) = if rotation & 1 == 1 { (h, w) } else { (w, h) };
    let dx = point[0] / dw * rw;
    let dy = point[1] / dh * rh;
    match rotation & 3 {
        0 => [dx, h - dy],
        1 => [dy, dx],
        2 => [w - dx, dy],
        _ => [w - dy, h - dx],
    }
}

/// Respiro entre o marcador da nota e o post-it (px CSS).
pub(crate) const POSTIT_GAP: f32 = 6.0;
/// Margem mínima do post-it dentro da janela (px CSS).
pub(crate) const POSTIT_MARGIN: f32 = 8.0;
/// Arrasto de nota só vale depois disto (px CSS): abaixo disso é clique
/// (abre a edição), como o clique-vs-arrasto da seleção.
const NOTE_DRAG_MIN_PX: f32 = 4.0;

/// Prende o post-it à janela: com o tamanho dado, o canto nunca sai da tela
/// nem cola na borda (nota perto do limite abre deslocada para dentro).
pub(crate) fn clamp_postit(pos: [f32; 2], size: [f32; 2], window: [f32; 2]) -> [f32; 2] {
    let axis = |p: f32, s: f32, w: f32| {
        let hi = (w - s - POSTIT_MARGIN).max(POSTIT_MARGIN);
        p.clamp(POSTIT_MARGIN, hi)
    };
    [
        axis(pos[0], size[0], window[0]),
        axis(pos[1], size[1], window[1]),
    ]
}

/// Lado mínimo/máximo do marcador compacto da nota (px CSS).
pub(crate) const NOTE_MARKER_MIN_PX: f32 = 6.0;
pub(crate) const NOTE_MARKER_MAX_PX: f32 = 14.0;
/// Folga de clique em volta do marcador (px CSS).
const NOTE_MARKER_HIT_PAD: f32 = 2.0;

/// Ponto da mídia original (espaço PDF, Y para cima) → ponto exibido (px CSS,
/// Y para baixo): a conta de `display_rect` para um ponto (o inverso de
/// `page_pt_at`). É o que alinha o marcador desenhado com o seu hit-test.
pub(crate) fn display_pt(
    pt: [f32; 2],
    media: MediaBox,
    rotation: u8,
    dw: f32,
    dh: f32,
) -> [f32; 2] {
    let [x, y, _, _] = display_rect(
        Quad::from_rect(pt[0], pt[1], pt[0], pt[1]),
        media,
        rotation,
        dw,
        dh,
    );
    [x, y]
}

/// Lado do marcador compacto da nota (px CSS): o `clamp` da altura do
/// primeiro quad — desenho e hit-test saem daqui, então coincidem.
pub(crate) fn marker_side(quads: &[Quad], media: MediaBox, rotation: u8, dw: f32, dh: f32) -> f32 {
    quads
        .first()
        .map(|q| {
            display_rect(*q, media, rotation, dw, dh)[3]
                .clamp(NOTE_MARKER_MIN_PX, NOTE_MARKER_MAX_PX)
        })
        .unwrap_or(NOTE_MARKER_MIN_PX)
}

/// Canto do marcador para um trecho sem nota ainda (default: origem do
/// primeiro quad) — o mesmo que `Annotation::marker_pt` com `marker: None`.
pub(crate) fn derived_marker_pt(quads: &[Quad]) -> [f32; 2] {
    quads
        .first()
        .map(|q| {
            let (left, _, _, top) = quad_bbox(*q);
            [left, top]
        })
        .unwrap_or([0.0, 0.0])
}

/// Prende o marcador à página: o deslocamento do arrasto para na mídia (mover
/// é sempre dentro de uma página).
pub(crate) fn clamped_marker_pt(pt: [f32; 2], media: MediaBox) -> [f32; 2] {
    [
        pt[0].clamp(0.0, media.width.max(1.0)),
        pt[1].clamp(0.0, media.height.max(1.0)),
    ]
}

#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub page: PageNo,
    pub range: TextRange,
    pub quad: Quad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub page: PageNo,
    pub range: TextRange,
}

/// Âncora do press (click-vs-drag no PointerUp); `exact=false` = press no
/// vazio com snap no glifo mais próximo (clique solto desseleciona).
#[derive(Debug, Clone, PartialEq, Eq)]
struct PressAnchor {
    sel: Selection,
    exact: bool,
}

/// Tipo de marcação de texto (issue #30, v1 sem persistência).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotKind {
    Highlight,
    Underline,
    Strikeout,
    /// Nota ancorada a um trecho: `Annotation.text` guarda o conteúdo.
    Note,
}

/// Marcação sobre um trecho: um retângulo por linha do texto (nunca um
/// bloco único — padrão dos leitores), em espaço da mídia original.
#[derive(Debug, Clone)]
pub struct Annotation {
    pub id: u64,
    pub page: PageNo,
    pub range: TextRange,
    pub quads: Vec<Quad>,
    pub kind: AnnotKind,
    /// Texto da nota ("" para highlight/underline/strikeout).
    pub text: String,
    /// Canto do marcador da nota (espaço de página, mesma página), quando ele
    /// foi arrastado para fora do trecho; `None` = na origem do primeiro quad.
    /// O trecho (`quads`/`range`) nunca se move: só o ícone amarelo anda.
    pub marker: Option<[f32; 2]>,
}

impl Annotation {
    /// Canto superior esquerdo do marcador no espaço de página: o dele
    /// (`marker`) ou a origem do primeiro quad (default das notas novas).
    pub(crate) fn marker_pt(&self) -> [f32; 2] {
        match self.marker {
            Some(pt) => pt,
            None => derived_marker_pt(&self.quads),
        }
    }
}

#[derive(Debug, Clone)]
enum AnnotAction {
    Add(Annotation),
    Remove(Annotation),
}

/// Rascunho de nota em edição (issue #30): ancorado a um trecho, com o
/// texto digitado e (`editing: Some(id)`) a nota que está sendo editada —
/// `None` = criando uma nova. Some junto com `annotations` ao abrir.
///
#[derive(Debug)]
pub struct NoteDraft {
    pub page: PageNo,
    pub range: TextRange,
    pub quads: Vec<Quad>,
    pub content: text_editor::Content,
    pub editing: Option<u64>,
    /// Canto do post-it na janela (px CSS) no instante em que abriu; a
    /// posição desenhada desconta a rolagem desde então (`anchor_scroll`).
    anchor: [f32; 2],
    anchor_scroll: f32,
}

impl Clone for NoteDraft {
    /// O conteúdo é o editor do iced (`text_editor::Content`), que não é
    /// `Clone`: a cópia nasce do texto, com o cursor no fim. O único clone de
    /// estado do app é o `Message::Opened`, de um documento recém-carregado —
    /// sem rascunho aberto.
    fn clone(&self) -> Self {
        Self {
            page: self.page,
            range: self.range,
            quads: self.quads.clone(),
            content: text_editor::Content::with_text(&self.content.text()),
            editing: self.editing,
            anchor: self.anchor,
            anchor_scroll: self.anchor_scroll,
        }
    }
}

/// Arrasto do marcador de uma nota em curso (press no marcador + arrasto além
/// do limiar): posição do marcador no press + deslocamento em pontos de
/// página. Só o ícone amarelo anda; o trecho marcado fica onde está.
#[derive(Debug, Clone)]
pub(crate) struct NoteDrag {
    id: u64,
    page: PageNo,
    from: [f32; 2],
    /// Canto do marcador no press (`Annotation::marker_pt`).
    marker0: [f32; 2],
    delta: [f32; 2],
}

impl NoteDrag {
    /// Canto candidato do marcador: posição do press + delta, preso à página.
    /// Ghost e resultado do soltar saem daqui — o que se vê é o que fica.
    fn candidate(&self, media: MediaBox) -> [f32; 2] {
        clamped_marker_pt(
            [
                self.marker0[0] + self.delta[0],
                self.marker0[1] + self.delta[1],
            ],
            media,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavCmd {
    Previous,
    Next,
    First,
    Last,
    GoTo(PageNo),
}

/// Teclas da árvore do sumário: ↑/↓ movem o cursor, Enter salta para a página.
/// Sem a aba Sumário aberta o handler ignora (como R/H/U/S sem seleção).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineKey {
    Prev,
    Next,
    Activate,
}
/// Pilha de páginas visitadas, como um navegador: `current()` é onde o leitor
/// está. Separada do `Ready` para poder ser testada sem documento.
#[derive(Debug, Clone)]
struct History {
    pages: Vec<PageNo>,
    pos: usize,
}

impl History {
    fn new(page: PageNo) -> Self {
        Self {
            pages: vec![page],
            pos: 0,
        }
    }

    /// Tamanho da pilha (inclui o "futuro" ainda não descartado).
    fn len(&self) -> usize {
        self.pages.len()
    }

    fn current(&self) -> PageNo {
        self.pages[self.pos]
    }

    /// Visita efetiva: descarta o "futuro" e ignora repetição consecutiva.
    fn visit(&mut self, page: PageNo) {
        if page == self.current() {
            return;
        }
        self.pages.truncate(self.pos + 1);
        self.pages.push(page);
        self.pos = self.pages.len() - 1;
    }

    /// Um passo (`forward` = avançar); `None` no limite.
    fn step(&mut self, forward: bool) -> Option<PageNo> {
        let next = if forward {
            self.pos.checked_add(1)?
        } else {
            self.pos.checked_sub(1)?
        };
        if next >= self.pages.len() {
            return None;
        }
        self.pos = next;
        Some(self.pages[next])
    }

    /// Reinicia a pilha numa página (abrir documento ou restaurar posição).
    fn reset(&mut self, page: PageNo) {
        *self = Self::new(page);
    }

    fn can_back(&self) -> bool {
        self.pos > 0
    }

    fn can_forward(&self) -> bool {
        self.pos + 1 < self.pages.len()
    }
}

pub enum Session {
    Empty(EmptyState),
    Loading {
        source: OpenSource,
        recents: Vec<PathBuf>,
        gen: u64,
        theme: Theme,
        /// DPR da janela (1.0 = sem Retina). Via `WindowScale`, como o tema.
        render_scale: f32,
        /// Tique da barra indeterminada de abertura (issue #41): só conta,
        /// não mede — o ciclo é fechado na view (`view::LOADING_STEPS`).
        phase: u16,
    },
    Ready(Tabs),
    Failed {
        source: OpenSource,
        message: String,
        recents: Vec<PathBuf>,
        gen: u64,
        theme: Theme,
        /// DPR da janela (1.0 = sem Retina). Via `WindowScale`, como o tema.
        render_scale: f32,
    },
}

/// Abas da janela (issue #40): uma por documento aberto. `docs` nunca fica
/// vazio e `active` sempre indexa uma aba válida — fechar a última aba volta
/// para `Session::Empty`.
///
/// A aba ativa é a que a vista desenha e a única que o `Session` enxerga:
/// `Deref`/`DerefMut` entregam o `Ready` ativo, e cada aba guarda o seu
/// estado de leitura (página, zoom, histórico, busca, marcações, painéis).
/// O que é da janela (viewport, tema, DPR, recentes) é mantido igual nas abas.
///
#[derive(Debug, Clone)]
pub struct Tabs {
    docs: Vec<Ready>,
    active: usize,
    /// Geração + origem do documento carregando para uma aba nova (⌘T ou
    /// abrir com uma aba já aberta). `None` = nada pendente.
    pending: Option<(u64, OpenSource)>,
    /// Falha ao abrir a aba pendente; a janela segue com as abas que tinha e
    /// a mensagem aparece na faixa de abas.
    open_error: Option<String>,
}

/// Altura reservada pela faixa de abas: zero com um documento só (a janela de
/// hoje fica idêntica) e `view::TAB_STRIP_HEIGHT` com duas ou mais.
fn strip_height(count: usize) -> f32 {
    if count > 1 {
        crate::view::TAB_STRIP_HEIGHT
    } else {
        0.0
    }
}

impl Tabs {
    /// Janela com um documento só.
    pub fn single(ready: Ready) -> Self {
        Self {
            docs: vec![ready],
            active: 0,
            pending: None,
            open_error: None,
        }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Abas na ordem da faixa (a ativa inclusa).
    pub fn docs(&self) -> &[Ready] {
        &self.docs
    }

    pub fn open_error(&self) -> Option<&str> {
        self.open_error.as_deref()
    }

    /// A aba ativa (a vista desenha esta).
    pub(crate) fn active(&self) -> &Ready {
        &self.docs[self.active]
    }

    fn active_mut(&mut self) -> &mut Ready {
        let active = self.active;
        &mut self.docs[active]
    }

    fn pending_gen(&self) -> Option<u64> {
        self.pending.as_ref().map(|(gen, _)| *gen)
    }

    /// Documento dono de uma resposta assíncrona, em qualquer aba: o render (ou
    /// a impressão) volta para a aba que o pediu, mesmo que a ativa já seja
    /// outra.
    fn by_gen(&mut self, gen: u64) -> Option<&mut Ready> {
        self.docs.iter_mut().find(|doc| doc.open_gen == gen)
    }

    /// Maior geração em uso; a próxima aba recebe `max + 1`. As gerações não se
    /// repetem porque é por elas que as respostas assíncronas se acham.
    fn max_gen(&self) -> u64 {
        let docs = self.docs.iter().map(|doc| doc.open_gen);
        docs.chain(self.pending_gen()).max().unwrap_or(0)
    }

    /// Toda aba vê a mesma janela (as inativas não recebem evento de resize).
    fn set_viewport(&mut self, viewport: Viewport) {
        for doc in &mut self.docs {
            doc.viewport = viewport;
        }
    }

    fn set_theme(&mut self, theme: Theme) {
        for doc in &mut self.docs {
            doc.theme = theme;
        }
    }

    fn set_render_scale(&mut self, scale: f32) {
        for doc in &mut self.docs {
            doc.render_scale = scale;
        }
    }

    /// Aba nova entrou: vira a ativa e reserva a faixa (o painel encolhe uma
    /// vez). A aba nova copia a geometria da janela da ativa — o carregamento
    /// não passa por evento de resize.
    fn push(&mut self, mut ready: Ready) {
        ready.viewport = self.active().viewport;
        let before = self.docs.len();
        self.pending = None;
        self.open_error = None;
        self.docs.push(ready);
        self.active = self.docs.len() - 1;
        self.sync_strip(before);
    }

    /// Fecha a aba `index` e devolve o documento que sai (o chamador solta o
    /// motor dele). O foco vai para a vizinha da direita — ou para a última,
    /// quando a fechada era a do fim. A última aba é do chamador.
    fn remove(&mut self, index: usize) -> Ready {
        let before = self.docs.len();
        let gone = self.docs.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.docs.len() - 1);
        self.sync_strip(before);
        gone
    }

    fn select(&mut self, index: usize) {
        if index < self.docs.len() {
            self.active = index;
        }
    }

    /// Ctrl+Tab (e Ctrl+Shift+Tab) dão a volta na faixa.
    fn cycle(&mut self, step: i32) {
        let len = self.docs.len() as i32;
        self.active = (self.active as i32 + step).rem_euclid(len) as usize;
    }

    /// A faixa só existe com 2+ abas: ao cruzar o limite de uma para duas (e
    /// de volta) o painel do documento ganha/perde `TAB_STRIP_HEIGHT`.
    fn sync_strip(&mut self, before: usize) {
        let delta = strip_height(self.docs.len()) - strip_height(before);
        if delta == 0.0 {
            return;
        }
        for doc in &mut self.docs {
            doc.viewport.height = (doc.viewport.height - delta).max(1.0);
        }
    }
}

impl std::ops::Deref for Tabs {
    type Target = Ready;

    fn deref(&self) -> &Ready {
        self.active()
    }
}

impl std::ops::DerefMut for Tabs {
    fn deref_mut(&mut self) -> &mut Ready {
        self.active_mut()
    }
}

#[derive(Clone)]
pub struct Ready {
    pub source: OpenSource,
    engine: PdfiumEngine,
    pages: PageCatalog,
    pub signatures: PdfAnalysis,
    pub zoom: Zoom,
    pub visible: PageNo,
    /// Histórico voltar/avançar; `current()` acompanha `visible` (página alcançada
    /// por rolagem entra na pilha ao andar). Restaurar posição zera a pilha.
    history: History,
    /// Página única ou rolagem contínua (⋯ → Modo de página); restaurada ao abrir.
    pub view_mode: ViewMode,
    /// Vista girada em quartos de volta horários (0..=3, sessão; zera ao abrir).
    pub view_rotation: u8,
    /// Draft 1-based page number shown in the nav pill.
    page_input: String,
    pub search: Search,
    pub selection: Option<Selection>,
    /// Marcações da sessão (issue #30); zera ao abrir. Sem persistência na v1.
    pub annotations: Vec<Annotation>,
    next_annot_id: u64,
    /// Âncora do press (click-vs-drag no PointerUp); zera ao trocar de documento.
    press_anchor: Option<PressAnchor>,
    annot_undo: Vec<AnnotAction>,
    annot_redo: Vec<AnnotAction>,
    /// Rascunho de nota aberto (issue #30); zera ao abrir. Sem persistência na v1.
    pub note_draft: Option<NoteDraft>,
    /// Arrasto do marcador de uma nota em curso (mover); `None` = nenhum.
    note_drag: Option<NoteDrag>,
    /// Origem da folha na janela (px CSS) e rolagem do documento no último
    /// press: a vista manda no `PointerDown` (só ela conhece o layout).
    sheet_at: [f32; 2],
    sheet_scroll: f32,
    pub signatures_open: bool,
    pub pages_open: bool,
    /// Aba Sumário ativa no painel de Páginas (só existe se houver outline).
    pub outline_open: bool,
    pub outline: Option<Outline>,
    /// Caminhos colapsados na árvore (`[0, 2]` = 3º filho do 1º item).
    pub outline_collapsed: HashSet<Vec<usize>>,
    /// Linha sob o cursor do teclado (↑/↓); `None` = nunca andou, usa a ativa.
    outline_cursor: Option<Vec<usize>>,
    /// Evita disparar mais de um task de carregamento de outline por documento.
    outline_load_issued: bool,
    /// Tema Kiri — sobrevive a `begin_open`/`finish_open`/`close_document`.
    pub theme: Theme,
    /// Menu ⋯ aberto. Só existe em `Ready`; zera ao trocar de documento.
    pub overflow_open: bool,
    /// Diálogo de impressão aberto (`None` = fechado). Só existe em `Ready`.
    pub print_dialog: Option<PrintDialog>,
    /// Linha de status pós-envio ("Enviado para …"); limpa ao reabrir o diálogo.
    pub print_status: Option<String>,
    /// Linha de status do salvamento de cópia ("Cópia salva em …"); limpa ao
    /// abrir/trocar de documento e ao reabrir o fluxo.
    pub save_status: Option<String>,
    /// Aviso de documento assinado aberto (Salvar mesmo assim / Voltar).
    /// Só o modal fecha; o estado zera ao escolher qualquer um dos botões.
    pub save_warning: bool,
    pub pages_scroll_y: f32,
    /// Offset Y do painel do documento (só contínuo); deriva `visible`.
    pub doc_scroll_y: f32,
    recents: Vec<PathBuf>,
    /// DPR da janela: bitmap sai em px físicos (zoom CSS × isto).
    pub render_scale: f32,
    open_gen: u64,
    /// Invalidates in-flight renders on nav/zoom/DPI changes.
    render_gen: u64,
    surfaces: SurfaceCache,
    thumbs: ThumbCache,
    render_inflight: HashSet<(u32, u16, u8)>,
    render_inflight_gen: HashMap<(u32, u16, u8), u64>,
    render_inflight_doc: HashMap<(u32, u16, u8), u64>,
    failed: HashSet<(u32, u16, u8)>,
    page_data_inflight: HashSet<u32>,
    page_data_failed: HashSet<u32>,
    viewport: Viewport,
}

/// Modo de intervalo do diálogo (v1: sem lista livre tipo "1-3, 5").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RangeMode {
    #[default]
    All,
    Current,
    Custom,
}

/// Estado do diálogo de impressão próprio (⋯ → Imprimir).
#[derive(Debug, Clone)]
pub struct PrintDialog {
    pub printers: Vec<PrinterInfo>,
    pub printers_loading: bool,
    pub selected: Option<usize>,
    pub range_mode: RangeMode,
    /// Campos "De"/"Até" em 1-based, como o usuário digitou.
    pub from_input: String,
    pub to_input: String,
    pub copies: u32,
    pub orientation: PrintOrientation,
    /// Índice na lista resolvida (preview "página X de Y").
    pub preview: usize,
    pub busy: bool,
    pub error: Option<String>,
}

impl PrintDialog {
    fn fresh(page_count: u32, current: PageNo) -> Self {
        let current_1 = current.index().saturating_add(1).min(page_count.max(1));
        Self {
            printers: Vec::new(),
            printers_loading: true,
            selected: None,
            range_mode: RangeMode::default(),
            from_input: current_1.to_string(),
            to_input: current_1.to_string(),
            copies: 1,
            orientation: PrintOrientation::default(),
            preview: 0,
            busy: false,
            error: None,
        }
    }

    pub fn selected_printer(&self) -> Option<&PrinterInfo> {
        self.selected.and_then(|index| self.printers.get(index))
    }

    /// Valida o diálogo → seleção assável no PDF.
    pub fn selection(&self, page_count: u32, current: PageNo) -> Result<PrintSelection, String> {
        let range = match self.range_mode {
            RangeMode::All => PrintRange::All,
            RangeMode::Current => PrintRange::Current(current),
            RangeMode::Custom => {
                let from = parse_1based(&self.from_input, page_count)?;
                let to = parse_1based(&self.to_input, page_count)?;
                if from > to {
                    return Err("«De» maior que «Até»".into());
                }
                PrintRange::FromTo {
                    from: PageNo::from_index(from - 1),
                    to: PageNo::from_index(to - 1),
                }
            }
        };
        Ok(PrintSelection {
            range,
            copies: self.copies.clamp(1, MAX_COPIES),
            orientation: self.orientation,
        })
    }

    /// Páginas do preview; intervalo inválido mostra só a atual.
    pub fn preview_pages(&self, page_count: u32, current: PageNo) -> Vec<PageNo> {
        self.selection(page_count, current)
            .ok()
            .and_then(|selection| crate::print::resolve_range(selection.range, page_count).ok())
            .filter(|pages| !pages.is_empty())
            .unwrap_or_else(|| vec![current])
    }
}

fn print_job_title(ready: &Ready) -> String {
    ready
        .source
        .path()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("documento")
        .to_string()
}

/// Puro: o documento tem assinatura digital que a cópia marcada invalidaria?
/// (Decisão separada do modal para poder testar sem rfd.)
fn needs_sign_warning(signatures: &PdfAnalysis) -> bool {
    !signatures.signatures.is_empty()
}

/// Puro: o destino é o próprio original? O original nunca é sobrescrito —
/// canonicaliza quando dá (caminhos relativos, symlinks) e cai para a
/// comparação direta quando não (arquivo ainda inexistente).
fn is_same_file(dest: &std::path::Path, src: &std::path::Path) -> bool {
    if dest == src {
        return true;
    }
    match (dest.canonicalize(), src.canonicalize()) {
        (Ok(dest), Ok(src)) => dest == src,
        _ => false,
    }
}

/// Aplica as marcações ao documento aberto na worker e grava a cópia em
/// `dest`. `PdfiumEngine` é `Clone + Send + Sync` (só um `Arc<Shared>` com
/// canal `mpsc`), então atravessa o `spawn_blocking` como na impressão.
async fn save_copy_file(
    engine: PdfiumEngine,
    annotations: Vec<Annotation>,
    dest: PathBuf,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let bytes = engine.save_copy(&annotations).map_err(|e| e.to_string())?;
        std::fs::write(&dest, bytes).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|join| Err(join.to_string()))
}

/// Diálogo nativo de destino da cópia (bloqueante, como o do abrir).
/// Cancelado não escreve nada; escolhido dispara `save_copy_file`.
fn start_save_dialog(ready: &mut Ready) -> Task<Message> {
    let source = ready.source.path().to_path_buf();
    let Some(dest) = rfd::FileDialog::new()
        .add_filter("PDF", &["pdf"])
        .set_file_name(suggested_marked_name(&source))
        .save_file()
    else {
        return Task::none();
    };
    if is_same_file(&dest, &source) {
        ready.save_status = Some("Escolha outro nome — o original nunca é sobrescrito.".into());
        return Task::none();
    }
    ready.save_status = None;
    let annotations = ready.annotations.clone();
    let doc_gen = ready.open_gen;
    let engine = ready.engine.clone();
    Task::perform(
        save_copy_file(engine, annotations, dest.clone()),
        move |result| Message::SaveCopyDone {
            doc_gen,
            path: dest.clone(),
            result,
        },
    )
}

fn parse_1based(input: &str, page_count: u32) -> Result<u32, String> {
    input
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|page| (1..=page_count.max(1)).contains(page))
        .ok_or_else(|| format!("use páginas de 1 até {}", page_count.max(1)))
}

struct PageCatalog {
    total: u32,
    media: Vec<Option<MediaBox>>,
    text: Vec<Option<TextLayer>>,
}

impl Clone for PageCatalog {
    fn clone(&self) -> Self {
        Self {
            total: self.total,
            media: self.media.clone(),
            text: self.text.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct CachedSurface {
    pub scale: Scale,
    pub rotation: u8,
    pub image: image::Handle,
    rgba_bytes: usize,
}

impl CachedSurface {
    fn from_page(surface: PageSurface, rotation: u8) -> Self {
        let rgba_bytes = surface.bitmap.rgba.len();
        let image = image::Handle::from_rgba(
            surface.bitmap.width,
            surface.bitmap.height,
            surface.bitmap.rgba,
        );
        Self {
            scale: surface.scale,
            rotation: rotation & 3,
            image,
            rgba_bytes,
        }
    }

    fn byte_size(&self) -> usize {
        self.rgba_bytes
    }
}

#[derive(Clone, Default)]
struct SurfaceCache {
    pages: HashMap<u32, CachedSurface>,
}

impl SurfaceCache {
    fn get(&self, page: PageNo, scale: Scale, rotation: u8) -> Option<&CachedSurface> {
        let cached = self.pages.get(&page.index())?;
        if cached.scale == scale && cached.rotation == rotation & 3 {
            Some(cached)
        } else {
            None
        }
    }

    /// Stale-while-revalidate: devolve o bitmap armazenado enquanto o render da
    /// nova escala não chega (troca de zoom/viewport).
    fn fallback_for_page(&self, page: PageNo) -> Option<&CachedSurface> {
        self.pages.get(&page.index())
    }

    fn insert(&mut self, page: PageNo, _scale: Scale, rotation: u8, surface: PageSurface) {
        self.pages
            .insert(page.index(), CachedSurface::from_page(surface, rotation));
    }

    fn retain_pages(&mut self, keep: &HashSet<u32>) {
        self.pages.retain(|page, _| keep.contains(page));
    }

    fn neighbor_bytes(&self, visible: u32) -> usize {
        self.pages
            .iter()
            .filter(|(page, _)| **page != visible)
            .map(|(_, surface)| surface.byte_size())
            .sum()
    }

    fn page_bytes(&self, page: u32) -> usize {
        self.pages
            .get(&page)
            .map(CachedSurface::byte_size)
            .unwrap_or(0)
    }

    fn enforce_neighbor_budget(&mut self, visible: u32, budget: usize) {
        while self.neighbor_bytes(visible) > budget {
            let victim = self
                .pages
                .iter()
                .filter(|(page, _)| **page != visible)
                .max_by_key(|(page, _)| page.abs_diff(visible))
                .map(|(page, _)| *page);
            let Some(page) = victim else {
                break;
            };
            self.pages.remove(&page);
        }
    }
}

#[derive(Clone, Default)]
struct ThumbCache {
    entries: HashMap<(u32, u16), CachedSurface>,
}

impl ThumbCache {
    fn get(&self, page: PageNo, scale: Scale) -> Option<&CachedSurface> {
        self.entries.get(&(page.index(), scale.key()))
    }

    fn insert(&mut self, page: PageNo, scale: Scale, surface: PageSurface) {
        let idx = page.index();
        self.entries.retain(|(page, _), _| *page != idx);
        self.entries
            .insert((idx, scale.key()), CachedSurface::from_page(surface, 0));
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn retain_pages(&mut self, keep: &HashSet<u32>) {
        self.entries.retain(|(page, _), _| keep.contains(page));
    }
}

fn estimated_rgba_bytes(media: MediaBox, scale: Scale) -> usize {
    let w = (media.width.max(1.0) * scale.factor()).round().max(1.0) as u64;
    let h = (media.height.max(1.0) * scale.factor()).round().max(1.0) as u64;
    (w * h * 4).min(usize::MAX as u64) as usize
}

fn neighbor_page_set(visible: u32, total: u32) -> HashSet<u32> {
    let mut keep = HashSet::new();
    if total == 0 {
        return keep;
    }
    keep.insert(visible);
    if visible > 0 {
        keep.insert(visible - 1);
    }
    if visible + 1 < total {
        keep.insert(visible + 1);
    }
    keep
}

fn render_key(page: PageNo, scale: Scale, rotation: u8) -> (u32, u16, u8) {
    (page.index(), scale.key(), rotation & 3)
}

#[derive(Debug, Clone)]
pub enum Message {
    PickFile,
    FileDropped(PathBuf),
    Opened {
        gen: u64,
        result: Result<Ready, OpenError>,
    },
    /// Tique da tela de abertura (issue #41): avança a barra indeterminada.
    LoadingTick,
    PageData {
        page: PageNo,
        doc_gen: u64,
        result: Result<(MediaBox, TextLayer), String>,
    },
    Close,
    /// Fecha a aba ativa (⌘W); se for a última, fecha a janela (→ `Empty`).
    CloseTabActive,
    /// Fecha a aba `usize` (o × da faixa de abas).
    CloseTab(usize),
    /// Troca a aba ativa (clique na faixa de abas).
    SelectTab(usize),
    /// Aba seguinte (`+1`) ou anterior (`-1`) — Ctrl+Tab / Ctrl+Shift+Tab.
    CycleTab(i32),
    Nav(NavCmd),
    PageInput(String),
    PageSubmit,
    SetZoom(Zoom),
    /// Gira a vista 90° no sentido horário (ciclo 0→1→2→3→0).
    RotateView,
    SetViewport(Viewport),
    SearchChanged(String),
    PointerDown {
        page: PageNo,
        page_pt: [f32; 2],
        /// Canto superior esquerdo da folha na janela (px CSS) — só a vista
        /// sabe; é o que ancora o post-it no lugar da nota.
        sheet: [f32; 2],
    },
    PointerMove {
        page: PageNo,
        page_pt: [f32; 2],
    },
    PointerUp {
        page: PageNo,
        page_pt: [f32; 2],
    },
    CopySelection,
    /// Marca a seleção atual (H/U/S ou menu ⋯); ignora sem seleção.
    Annotate(AnnotKind),
    /// Janela perdeu o foco no meio do drag: o Up nunca chega, então a
    /// âncora morre aqui (senão o hover passa a estender a seleção).
    DragCancelled,
    AnnotUndo,
    AnnotRedo,
    /// Edição no post-it aberto (o editor do iced manda a ação; o rascunho
    /// aplica e o texto vive em `NoteDraft.content`).
    NoteEdit(text_editor::Action),
    /// Salva o rascunho como nota (cria nova ou substitui a que está em
    /// edição); texto vazio/só-espaço descarta sem criar.
    NoteSave,
    /// Fecha o rascunho sem criar/alterar nada.
    NoteCancel,
    /// Botão vermelho do post-it (só em edição): remove a nota que está sendo
    /// editada e fecha o rascunho; Ctrl+Z desfaz como qualquer marcação.
    NoteDelete,
    Rendered {
        page: PageNo,
        scale: Scale,
        rotation: u8,
        doc_gen: u64,
        render_gen: u64,
        surface: Option<PageSurface>,
    },
    ToggleSignatures,
    TogglePages,
    ToggleOverflow,
    /// Aba Sumário no painel de Páginas (`true` = sumário, `false` = miniaturas).
    OutlineTab(bool),
    /// Expande/colapsa um nó da árvore (caminho de índices desde a raiz).
    OutlineFold(Vec<usize>),
    /// Resultado do carregamento preguiçoso do sumário (outline) após abrir
    /// o documento. `None` indica que o PDF não possui outline; `doc_gen` diz
    /// de qual aba veio (a ativa pode ter mudado no meio).
    OutlineLoaded {
        doc_gen: u64,
        outline: Option<Outline>,
    },
    /// Clique em um item do sumário: navega (com clamp) para a página do item.
    OutlineJump(PageNo),
    /// ↑/↓/Enter na árvore do sumário (ignorado sem a aba aberta).
    OutlineKey(OutlineKey),
    /// ⋯ → Imprimir: abre o diálogo próprio e lista impressoras em background.
    OpenPrintDialog,
    /// Lista do SO pronta; pré-seleciona a default (ou a primeira).
    PrintersLoaded(Vec<PrinterInfo>),
    ClosePrintDialog,
    PrintSelectPrinter(usize),
    PrintSetRangeMode(RangeMode),
    PrintSetFromInput(String),
    PrintSetToInput(String),
    PrintCopiesPlus,
    PrintCopiesMinus,
    PrintSetOrientation(PrintOrientation),
    PrintPreviewPrev,
    PrintPreviewNext,
    /// Monta o PDF da seleção e faz spool; `doc_gen` precisa casar na volta.
    PrintSubmit,
    PrintSubmitted {
        doc_gen: u64,
        printer: String,
        result: Result<u64, String>,
    },
    /// Rota de fuga: abre o PDF da seleção no visualizador padrão.
    PrintOpenPdf,
    PrintPdfOpened {
        doc_gen: u64,
        result: Result<String, String>,
    },
    /// Captura cliques no cartão do modal (sem efeito); o fundo fecha o diálogo.
    PrintNop,
    /// ⋯ → Salvar cópia com marcações…: abre o diálogo de destino (ou o aviso
    /// de documento assinado antes dele).
    SaveCopyRequested,
    /// "Salvar mesmo assim" no aviso de documento assinado: segue para o
    /// diálogo de destino.
    SaveCopyConfirmed,
    /// "Voltar" (ou clique no fundo/Esc) no aviso de documento assinado.
    SaveCopyCancelled,
    /// Resultado da cópia assíncrona; `doc_gen` precisa casar na volta.
    SaveCopyDone {
        doc_gen: u64,
        path: PathBuf,
        result: Result<(), String>,
    },
    SetTheme(Theme),
    /// Janela informou tamanho + identidade: ajusta viewport e reconsulta o DPR.
    WindowMetrics {
        width: f32,
        height: f32,
        id: window::Id,
    },
    /// Densidade da janela (device pixels por px CSS). 1.0 = sem Retina.
    WindowScale(f32),
    PagesScrolled(f32),
    /// Histórico voltar/avançar (Alt+←/→; ⌘ no mac).
    HistoryBack,
    HistoryForward,
    /// Rolagem do painel do documento (só contínuo); deriva `visible`.
    DocScrolled(f32),
    /// ⋯ → Modo de página: página única ou rolagem contínua.
    SetViewMode(ViewMode),
    BrowseTo(Option<PathBuf>),
    ListingReady {
        path: Option<PathBuf>,
        result: Result<Vec<FsEntry>, String>,
    },
    OpenRecent(PathBuf),
    RecentsReady(Vec<PathBuf>),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum OpenError {
    #[error("não foi possível ler o arquivo: {0}")]
    Io(String),
    #[error("não foi possível abrir o PDF: {0}")]
    Engine(String),
    #[error("assinaturas: {0}")]
    Sign(String),
}

impl Session {
    pub fn empty() -> Self {
        Session::Empty(EmptyState {
            theme: read_theme(),
            render_scale: 1.0,
            ..EmptyState::default()
        })
    }

    pub fn open_path(path: PathBuf) -> Self {
        Session::Loading {
            source: OpenSource::Path(path),
            recents: read_recents(),
            gen: 1,
            theme: read_theme(),
            render_scale: 1.0,
            phase: 0,
        }
    }

    pub fn boot(self) -> (Self, Task<Message>) {
        match &self {
            Session::Loading { source, gen, .. } => {
                let source = source.clone();
                let gen = *gen;
                (
                    self,
                    Task::perform(open_ready(source), move |result| Message::Opened {
                        gen,
                        result,
                    }),
                )
            }
            Session::Empty(_) => (self, empty_tasks()),
            _ => (self, Task::none()),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PickFile => match OpenSource::from_dialog() {
                None => Task::none(),
                Some(source) => self.begin_open(source),
            },
            Message::FileDropped(path) => {
                if !is_pdf(&path) {
                    Task::none()
                } else {
                    self.begin_open(OpenSource::Dropped(path))
                }
            }
            Message::OpenRecent(path) => {
                if !is_pdf(&path) {
                    Task::none()
                } else {
                    self.begin_open(OpenSource::Path(path))
                }
            }
            Message::Opened { gen, result } => {
                self.apply_open(gen, result);
                // Modo contínuo restaurado: a primeira vista já fica na página.
                let follow = match self {
                    Session::Ready(ready) => nav_follow(ready),
                    _ => Task::none(),
                };
                Task::batch([self.schedule_work(), follow])
            }
            // Tela de abertura: só o contador da barra; nada mais reage a isto.
            Message::LoadingTick => {
                if let Session::Loading { phase, .. } = self {
                    *phase = phase.wrapping_add(1);
                }
                Task::none()
            }
            Message::PageData {
                page,
                doc_gen,
                result,
            } => {
                if let Session::Ready(tabs) = self {
                    let Some(ready) = tabs.by_gen(doc_gen) else {
                        return Task::none();
                    };
                    ready.page_data_inflight.remove(&page.index());
                    match result {
                        Ok((media, text)) => {
                            ready.page_data_failed.remove(&page.index());
                            let i = page.index() as usize;
                            if i < ready.pages.total as usize {
                                ready.pages.media[i] = Some(media);
                                if !ready.search.query().is_empty() {
                                    ready.search.extend_page(&text);
                                }
                                ready.pages.text[i] = Some(text);
                            }
                        }
                        Err(_) => {
                            ready.page_data_failed.insert(page.index());
                        }
                    }
                }
                self.schedule_work()
            }
            Message::Close => self.close_document(),
            Message::CloseTabActive => {
                let index = match self {
                    Session::Ready(tabs) => tabs.active_index(),
                    _ => return Task::none(),
                };
                self.close_tab(index)
            }
            Message::CloseTab(index) => self.close_tab(index),
            Message::SelectTab(index) => self.select_tab(index),
            Message::CycleTab(step) => self.cycle_tab(step),
            Message::Nav(cmd) => {
                let follow = if let Session::Ready(ready) = self {
                    if ready.print_dialog.is_some() {
                        // Com o modal aberto, as setas paginam o preview, não o documento.
                        let count = ready.page_count();
                        let current = ready.visible;
                        if let Some(dialog) = ready.print_dialog.as_mut() {
                            let pages = dialog.preview_pages(count, current);
                            let last = pages.len().saturating_sub(1);
                            dialog.preview = match cmd {
                                NavCmd::Previous => dialog.preview.saturating_sub(1),
                                NavCmd::Next => dialog.preview.saturating_add(1).min(last),
                                NavCmd::First => 0,
                                NavCmd::Last => last,
                                NavCmd::GoTo(page) => pages
                                    .iter()
                                    .position(|listed| *listed == page)
                                    .unwrap_or(dialog.preview)
                                    .min(last),
                            };
                        }
                    } else {
                        ready.apply_nav(cmd);
                    }
                    nav_follow(ready)
                } else {
                    Task::none()
                };
                Task::batch([self.schedule_work(), follow])
            }
            Message::PageInput(draft) => {
                if let Session::Ready(ready) = self {
                    ready.page_input = draft;
                }
                Task::none()
            }
            Message::PageSubmit => {
                let follow = if let Session::Ready(ready) = self {
                    ready.submit_page_input();
                    nav_follow(ready)
                } else {
                    Task::none()
                };
                Task::batch([self.schedule_work(), follow])
            }
            Message::SetZoom(zoom) => {
                let follow = if let Session::Ready(ready) = self {
                    ready.zoom = zoom;
                    ready.overflow_open = false;
                    ready.bump_render_gen();
                    ready.save_position();
                    nav_follow(ready)
                } else {
                    Task::none()
                };
                Task::batch([self.schedule_work(), follow])
            }
            Message::RotateView => {
                let follow = if let Session::Ready(ready) = self {
                    ready.view_rotation = (ready.view_rotation + 1) & 3;
                    ready.overflow_open = false;
                    ready.bump_render_gen();
                    nav_follow(ready)
                } else {
                    Task::none()
                };
                Task::batch([self.schedule_work(), follow])
            }
            Message::SetViewport(viewport) => {
                let follow = if let Session::Ready(ready) = self {
                    ready.viewport = viewport;
                    ready.bump_render_gen();
                    nav_follow(ready)
                } else {
                    Task::none()
                };
                Task::batch([self.schedule_work(), follow])
            }
            Message::WindowMetrics { width, height, id } => {
                // A faixa de abas (2+ documentos) tira altura do painel.
                let height = (height - self.tab_strip_height()).max(1.0);
                let follow = if let Session::Ready(tabs) = self {
                    // Toda aba vê a mesma janela: as inativas não recebem evento.
                    tabs.set_viewport(Viewport { width, height });
                    tabs.bump_render_gen();
                    nav_follow(tabs)
                } else {
                    Task::none()
                };
                Task::batch([self.schedule_work(), follow, query_window_scale(id)])
            }
            Message::WindowScale(scale) => {
                if !scale.is_finite() || scale < 1.0 {
                    return Task::none();
                }
                let scale = scale.min(4.0);
                if (self.render_scale() - scale).abs() < 0.001 {
                    return Task::none();
                }
                self.set_render_scale(scale);
                if let Session::Ready(ready) = self {
                    ready.bump_render_gen();
                }
                self.schedule_work()
            }
            Message::SearchChanged(query) => {
                let follow = if let Session::Ready(tabs) = self {
                    let ready = tabs.active_mut();
                    ready.set_query(query);
                    // Primeiro hit, sem segurar o empréstimo da busca.
                    if let Some(page) = ready.search.hits.first().map(|hit| hit.page) {
                        ready.navigate_to(page);
                    }
                    nav_follow(ready)
                } else {
                    Task::none()
                };
                Task::batch([self.schedule_work(), follow])
            }
            Message::PointerDown {
                page,
                page_pt,
                sheet,
            } => {
                if let Session::Ready(ready) = self {
                    // Origem da folha + rolagem do press: base da âncora do
                    // post-it que o clique pode abrir logo abaixo.
                    ready.sheet_at = sheet;
                    ready.sheet_scroll = ready.doc_scroll_y;
                    // Press sobre uma nota: candidato a arrasto. O clique
                    // (sem arrasto) abre a edição no PointerUp; a seleção
                    // fica de fora para o arrasto não estender texto.
                    if ready.begin_note_drag(page, page_pt) {
                        return Task::none();
                    }
                    match ready.pages.text.get(page.index() as usize) {
                        Some(Some(layer)) => {
                            // Press ancora até no vazio (snap no mais próximo);
                            // só o clique solto no vazio desseleciona (Up).
                            let exact = layer.hit(page_pt);
                            let snapped = exact.or_else(|| layer.hit_nearest(page_pt));
                            match snapped {
                                Some(i) => {
                                    let (start, end) = glyph_byte_range(layer, i);
                                    let sel = Selection {
                                        page,
                                        range: TextRange { start, end },
                                    };
                                    ready.press_anchor = Some(PressAnchor {
                                        sel: sel.clone(),
                                        exact: exact.is_some(),
                                    });
                                    ready.selection = Some(sel);
                                }
                                None => {
                                    ready.selection = None;
                                    ready.press_anchor = None;
                                }
                            }
                        }
                        _ => {
                            ready.selection = None;
                            ready.press_anchor = None;
                        }
                    }
                }
                Task::none()
            }
            Message::PointerMove { page, page_pt } => {
                if let Session::Ready(tabs) = self {
                    let ready = tabs.active_mut();
                    // Arrasto de nota: o delta mede o movimento desde o press
                    // (a folha já vem com o ponto preso à borda).
                    if ready.update_note_drag(page, page_pt) {
                        return Task::none();
                    }
                    if let Some(sel) = ready.selection.as_mut() {
                        if sel.page == page {
                            if let Some(Some(layer)) = ready.pages.text.get(page.index() as usize) {
                                // Snap ao glifo mais próximo: o arrasto segue
                                // o cursor mesmo no vão (o clique usa `hit`
                                // exato e desseleciona no vazio).
                                if let Some(i) = layer.hit_nearest(page_pt) {
                                    let cursor = glyph_byte_range(layer, i);
                                    let anchor = ready
                                        .press_anchor
                                        .as_ref()
                                        .filter(|a| a.sel.page == page)
                                        .map(|a| a.sel.range);
                                    // Âncora fixa no press: puxar de volta
                                    // encolhe (padrão dos leitores).
                                    sel.range = extend_range(anchor, cursor);
                                }
                            }
                        }
                    }
                }
                Task::none()
            }
            Message::PointerUp { page, page_pt } => {
                if let Session::Ready(ready) = self {
                    // Press sobre nota: soltar move (o ghost é o resultado) ou,
                    // sem passar do limiar, é o clique que abre a edição.
                    if ready.finish_note_drag(page, page_pt) {
                        return crate::view::focus_postit();
                    }
                    // Clique (press+release sem arrasto): sobre marcação
                    // remove; no vazio desseleciona; em glifo mantém a palavra.
                    // Arrasto só estende (já feito no PointerMove).
                    if let Some(anchor) = ready.press_anchor.clone() {
                        if Some(anchor.sel.clone()) == ready.selection {
                            if let Some(id) = ready.annotation_at(page, page_pt) {
                                // Clique sobre nota abre a edição dela; as
                                // demais marcações o clique remove.
                                if ready.open_note_draft_for_id(id) {
                                    return crate::view::focus_postit();
                                } else {
                                    ready.remove_annotation(id);
                                }
                            } else if !anchor.exact {
                                ready.selection = None;
                            }
                        }
                    }
                    ready.press_anchor = None;
                }
                Task::none()
            }
            Message::CopySelection => {
                if let Session::Ready(ready) = self {
                    ready.overflow_open = false;
                    if let Some(text) = ready.selection_plain_text() {
                        return clipboard::write(text);
                    }
                }
                Task::none()
            }
            Message::Annotate(kind) => {
                if let Session::Ready(ready) = self {
                    ready.overflow_open = false;
                    if kind == AnnotKind::Note {
                        // Nota: abre o rascunho da seleção e mantém a
                        // seleção (âncora do draft); sem seleção ignora.
                        if ready.open_note_draft() {
                            return crate::view::focus_postit();
                        }
                    } else if ready.annotate_selection(kind) {
                        // Pós-marcação limpa a seleção (padrão dos leitores).
                        ready.selection = None;
                        ready.press_anchor = None;
                    }
                }
                Task::none()
            }
            Message::DragCancelled => {
                if let Session::Ready(ready) = self {
                    ready.press_anchor = None;
                    // A folha perdeu o mouse (solta fora dela, janela sem
                    // foco): soltar fora da página cancela o arrasto.
                    ready.note_drag = None;
                }
                Task::none()
            }
            Message::AnnotUndo => {
                if let Session::Ready(ready) = self {
                    ready.overflow_open = false;
                    ready.annot_undo_once();
                }
                Task::none()
            }
            Message::AnnotRedo => {
                if let Session::Ready(ready) = self {
                    ready.overflow_open = false;
                    ready.annot_redo_once();
                }
                Task::none()
            }
            Message::NoteEdit(action) => {
                if let Session::Ready(ready) = self {
                    if let Some(draft) = ready.note_draft.as_mut() {
                        draft.content.perform(action);
                    }
                }
                Task::none()
            }
            Message::NoteSave => {
                if let Session::Ready(ready) = self {
                    ready.save_note_draft();
                }
                Task::none()
            }
            Message::NoteCancel => {
                if let Session::Ready(ready) = self {
                    ready.note_draft = None;
                }
                Task::none()
            }
            Message::NoteDelete => {
                if let Session::Ready(ready) = self {
                    ready.delete_draft_note();
                }
                Task::none()
            }
            Message::Rendered {
                page,
                scale,
                rotation,
                doc_gen,
                render_gen,
                surface,
            } => {
                if let Session::Ready(tabs) = self {
                    let Some(ready) = tabs.by_gen(doc_gen) else {
                        return Task::none();
                    };
                    let key = render_key(page, scale, rotation);
                    ready.render_inflight.remove(&key);
                    ready.render_inflight_gen.remove(&key);
                    ready.render_inflight_doc.remove(&key);
                    if render_gen != ready.render_gen {
                        ready.evict_unused();
                        return self.schedule_work();
                    }
                    match surface {
                        Some(surface) => {
                            ready.failed.remove(&key);
                            let current = ready.page_scale(page);
                            if ready.visible == page
                                && current == scale
                                && ready.view_rotation & 3 == rotation & 3
                            {
                                ready.surfaces.insert(page, scale, rotation, surface);
                            } else if ready.prefetch_target() == Some((page, scale, rotation & 3)) {
                                ready.surfaces.insert(page, scale, rotation, surface);
                            } else if ready.pages_open || ready.print_preview_target() == Some(page)
                            {
                                if let Some(media) = ready.loaded_media(page) {
                                    if ready.thumb_scale_for(media) == scale
                                        && (ready.thumb_page_window().contains(&page)
                                            || ready.print_preview_target() == Some(page))
                                    {
                                        ready.thumbs.insert(page, scale, surface);
                                    }
                                }
                            }
                        }
                        None => {
                            ready.failed.insert(key);
                        }
                    }
                    ready.evict_unused();
                }
                self.schedule_work()
            }
            Message::ToggleSignatures => {
                if let Session::Ready(ready) = self {
                    ready.signatures_open = !ready.signatures_open;
                }
                Task::none()
            }
            Message::TogglePages => {
                let restore = if let Session::Ready(ready) = self {
                    ready.pages_open = !ready.pages_open;
                    if !ready.pages_open {
                        ready.thumbs.clear();
                    }
                    ready.failed.clear();
                    ready.pages_open.then_some(ready.pages_scroll_y)
                } else {
                    None
                };
                let mut tasks = vec![self.schedule_work()];
                if let Some(y) = restore {
                    tasks.push(scrollable::scroll_to(
                        crate::view::pages_scroll_id(),
                        scrollable::AbsoluteOffset { x: 0.0, y },
                    ));
                }
                Task::batch(tasks)
            }
            Message::ToggleOverflow => {
                if let Session::Ready(ready) = self {
                    ready.overflow_open = !ready.overflow_open;
                }
                Task::none()
            }
            Message::OutlineLoaded { doc_gen, outline } => {
                if let Session::Ready(tabs) = self {
                    if let Some(ready) = tabs.by_gen(doc_gen) {
                        ready.outline = outline;
                    }
                }
                // O outline destrava o gate em schedule_work: sem reagendar,
                // nada mais é despachado e a folha trava em "Renderizando…".
                self.schedule_work()
            }
            Message::OutlineTab(show) => {
                if let Session::Ready(ready) = self {
                    // Só existe aba quando há outline; sem outline, força miniaturas.
                    ready.outline_open = show && ready.outline.is_some();
                }
                Task::none()
            }
            Message::OutlineFold(path) => {
                if let Session::Ready(ready) = self {
                    if !ready.outline_collapsed.remove(&path) {
                        // Colapsar esconde os filhos: o cursor volta para a ativa.
                        if ready
                            .outline_cursor
                            .as_ref()
                            .is_some_and(|c| c.starts_with(&path) && *c != path)
                        {
                            ready.outline_cursor = None;
                        }
                        ready.outline_collapsed.insert(path);
                    }
                }
                Task::none()
            }
            Message::OutlineJump(page) => self.outline_jump(page),
            Message::OutlineKey(cmd) => {
                let Session::Ready(ready) = self else {
                    return Task::none();
                };
                if !ready.outline_open || ready.outline.is_none() {
                    return Task::none();
                }
                match cmd {
                    OutlineKey::Prev | OutlineKey::Next => {
                        let next = matches!(cmd, OutlineKey::Next);
                        match ready.outline_step(next) {
                            // Rola junto (senão o cursor sai da janela): +1
                            // desconta o cabeçalho das abas, que ocupa uma
                            // linha e está dentro do mesmo scrollable.
                            Some(row) => scrollable::scroll_to(
                                crate::view::pages_scroll_id(),
                                scrollable::AbsoluteOffset {
                                    x: 0.0,
                                    y: (row as f32 + 1.0) * OUTLINE_ROW,
                                },
                            ),
                            None => Task::none(),
                        }
                    }
                    OutlineKey::Activate => match ready.outline_cursor_page() {
                        Some(page) => self.outline_jump(page),
                        None => Task::none(),
                    },
                }
            }
            Message::OpenPrintDialog => {
                let Session::Ready(ready) = self else {
                    return Task::none();
                };
                ready.overflow_open = false;
                ready.print_status = None;
                ready.print_dialog = Some(PrintDialog::fresh(ready.page_count(), ready.visible));
                Task::batch([
                    Task::perform(
                        async move {
                            tokio::task::spawn_blocking(list_printers)
                                .await
                                .unwrap_or_default()
                        },
                        Message::PrintersLoaded,
                    ),
                    self.schedule_work(),
                ])
            }
            Message::PrintersLoaded(printers) => {
                if let Session::Ready(ready) = self {
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        dialog.printers_loading = false;
                        dialog.selected = printers
                            .iter()
                            .position(|printer| printer.is_default)
                            .or(if printers.is_empty() { None } else { Some(0) });
                        dialog.printers = printers;
                    }
                }
                Task::none()
            }
            Message::ClosePrintDialog => {
                if let Session::Ready(ready) = self {
                    // Esc com rascunho de nota aberto também o fecha: o
                    // popover de nota não tem subscription própria e a
                    // função pura de teclas não vê estado (Esc → este
                    // diálogo); a guarda de foco é o `status` do iced.
                    ready.note_draft = None;
                    // Idem para o arrasto de nota e para o aviso de
                    // documento assinado.
                    ready.note_drag = None;
                    ready.save_warning = false;
                    ready.overflow_open = false;
                    // Enviando: ignora (Esc) para não perder o resultado na volta.
                    if ready
                        .print_dialog
                        .as_ref()
                        .is_none_or(|dialog| !dialog.busy)
                    {
                        // Esc sem diálogo: desseleciona (padrão dos leitores).
                        if ready.print_dialog.is_none() {
                            ready.selection = None;
                            ready.press_anchor = None;
                        }
                        ready.print_dialog = None;
                    }
                }
                Task::none()
            }
            Message::PrintSelectPrinter(index) => {
                if let Session::Ready(ready) = self {
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        if !dialog.busy && index < dialog.printers.len() {
                            dialog.selected = Some(index);
                            dialog.error = None;
                        }
                    }
                }
                Task::none()
            }
            Message::PrintSetRangeMode(mode) => {
                if let Session::Ready(ready) = self {
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        if !dialog.busy {
                            dialog.range_mode = mode;
                            dialog.preview = 0;
                            dialog.error = None;
                        }
                    }
                }
                // O alvo do preview pode ter mudado: agenda o thumb.
                self.schedule_work()
            }
            Message::PrintSetFromInput(input) => {
                if let Session::Ready(ready) = self {
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        if !dialog.busy {
                            dialog.from_input = input;
                            dialog.preview = 0;
                        }
                    }
                }
                self.schedule_work()
            }
            Message::PrintSetToInput(input) => {
                if let Session::Ready(ready) = self {
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        if !dialog.busy {
                            dialog.to_input = input;
                            dialog.preview = 0;
                        }
                    }
                }
                self.schedule_work()
            }
            Message::PrintCopiesPlus => {
                if let Session::Ready(ready) = self {
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        dialog.copies = dialog.copies.saturating_add(1).min(MAX_COPIES);
                    }
                }
                Task::none()
            }
            Message::PrintCopiesMinus => {
                if let Session::Ready(ready) = self {
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        dialog.copies = dialog.copies.saturating_sub(1).max(1);
                    }
                }
                Task::none()
            }
            Message::PrintSetOrientation(orientation) => {
                if let Session::Ready(ready) = self {
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        if !dialog.busy {
                            dialog.orientation = orientation;
                            dialog.error = None;
                        }
                    }
                }
                Task::none()
            }
            Message::PrintPreviewPrev => {
                if let Session::Ready(ready) = self {
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        dialog.preview = dialog.preview.saturating_sub(1);
                    }
                }
                self.schedule_work()
            }
            Message::PrintPreviewNext => {
                if let Session::Ready(ready) = self {
                    let count = ready.page_count();
                    let current = ready.visible;
                    if let Some(dialog) = ready.print_dialog.as_mut() {
                        let last = dialog.preview_pages(count, current).len().saturating_sub(1);
                        dialog.preview = dialog.preview.saturating_add(1).min(last);
                    }
                }
                self.schedule_work()
            }
            Message::PrintSubmit => {
                let Session::Ready(ready) = self else {
                    return Task::none();
                };
                if ready.print_dialog.as_ref().is_none_or(|dialog| dialog.busy) {
                    return Task::none();
                }
                let page_count = ready.page_count();
                let current = ready.visible;
                let doc_gen = ready.open_gen;
                let title = print_job_title(ready);
                let engine = ready.engine.clone();
                let Some(dialog) = ready.print_dialog.as_mut() else {
                    return Task::none();
                };
                let selection = match dialog.selection(page_count, current) {
                    Ok(selection) => selection,
                    Err(err) => {
                        dialog.error = Some(err);
                        return Task::none();
                    }
                };
                let Some(printer) = dialog.selected_printer().map(|info| info.name.clone()) else {
                    dialog.error = Some("nenhuma impressora selecionada".into());
                    return Task::none();
                };
                dialog.busy = true;
                dialog.error = None;
                let printer_msg = printer.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            match print_selection_pdf(&engine, selection) {
                                Ok(pdf) => {
                                    spool_pdf(&printer, &pdf, &title).map_err(|err| err.to_string())
                                }
                                Err(err) => Err(err.to_string()),
                            }
                        })
                        .await
                        .unwrap_or_else(|join| Err(join.to_string()))
                    },
                    move |result| Message::PrintSubmitted {
                        doc_gen,
                        printer: printer_msg.clone(),
                        result,
                    },
                )
            }
            Message::PrintSubmitted {
                doc_gen,
                printer,
                result,
            } => {
                if let Session::Ready(tabs) = self {
                    if let Some(ready) = tabs.by_gen(doc_gen) {
                        match result {
                            Ok(job) => {
                                ready.print_dialog = None;
                                ready.print_status =
                                    Some(format!("Enviado para {printer} (job {job})"));
                            }
                            Err(err) => {
                                if let Some(dialog) = ready.print_dialog.as_mut() {
                                    dialog.busy = false;
                                    dialog.error = Some(err);
                                }
                            }
                        }
                    }
                }
                // Impressão não toca o cache de páginas: nada a reagendar.
                Task::none()
            }
            Message::PrintOpenPdf => {
                let Session::Ready(ready) = self else {
                    return Task::none();
                };
                if ready.print_dialog.as_ref().is_none_or(|dialog| dialog.busy) {
                    return Task::none();
                }
                let page_count = ready.page_count();
                let current = ready.visible;
                let doc_gen = ready.open_gen;
                let source = ready.source.path().to_path_buf();
                let engine = ready.engine.clone();
                let Some(dialog) = ready.print_dialog.as_mut() else {
                    return Task::none();
                };
                let selection = match dialog.selection(page_count, current) {
                    Ok(selection) => selection,
                    Err(err) => {
                        dialog.error = Some(err);
                        return Task::none();
                    }
                };
                dialog.busy = true;
                dialog.error = None;
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            match print_selection_pdf(&engine, selection) {
                                Ok(pdf) => write_and_open_print_pdf(&pdf, &source)
                                    .map(|path| path.display().to_string())
                                    .map_err(|err| err.to_string()),
                                Err(err) => Err(err.to_string()),
                            }
                        })
                        .await
                        .unwrap_or_else(|join| Err(join.to_string()))
                    },
                    move |result| Message::PrintPdfOpened { doc_gen, result },
                )
            }
            Message::PrintNop => Task::none(),
            Message::PrintPdfOpened { doc_gen, result } => {
                if let Session::Ready(tabs) = self {
                    if let Some(ready) = tabs.by_gen(doc_gen) {
                        if let Some(dialog) = ready.print_dialog.as_mut() {
                            dialog.busy = false;
                            if let Err(err) = result {
                                dialog.error = Some(err);
                            }
                        }
                    }
                }
                Task::none()
            }
            Message::SaveCopyRequested => {
                let Session::Ready(ready) = self else {
                    return Task::none();
                };
                ready.overflow_open = false;
                ready.save_status = None;
                if ready.annotations.is_empty() {
                    ready.save_status = Some("Nada para salvar — marque o texto primeiro.".into());
                    return Task::none();
                }
                // Assinado: o aviso vem antes do diálogo (a cópia com
                // marcações invalida a assinatura).
                if needs_sign_warning(&ready.signatures) {
                    ready.save_warning = true;
                    return Task::none();
                }
                start_save_dialog(ready)
            }
            Message::SaveCopyConfirmed => {
                let Session::Ready(ready) = self else {
                    return Task::none();
                };
                ready.save_warning = false;
                start_save_dialog(ready)
            }
            Message::SaveCopyCancelled => {
                if let Session::Ready(ready) = self {
                    ready.save_warning = false;
                }
                Task::none()
            }
            Message::SaveCopyDone {
                doc_gen,
                path,
                result,
            } => {
                if let Session::Ready(tabs) = self {
                    if let Some(ready) = tabs.by_gen(doc_gen) {
                        match result {
                            Ok(()) => {
                                // Registra a cópia nos recentes, mas não a abre:
                                // abrir descartaria o estado da sessão atual.
                                let recents = merge_recents(ready.recents.clone(), read_recents());
                                let recents = push_recent(recents, path.clone());
                                let _ = save_recents(&recents);
                                ready.recents = recents;
                                let name = path
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.display().to_string());
                                ready.save_status = Some(format!("Cópia salva em {name}"));
                            }
                            Err(err) => {
                                ready.save_status = Some(format!("Falha ao salvar: {err}"));
                            }
                        }
                    }
                }
                Task::none()
            }
            Message::SetTheme(theme) => {
                self.set_theme(theme);
                self.close_overflow();
                let _ = save_theme(theme);
                Task::none()
            }
            Message::PagesScrolled(y) => {
                if let Session::Ready(ready) = self {
                    ready.pages_scroll_y = y;
                }
                self.schedule_work()
            }
            Message::HistoryBack => {
                if let Session::Ready(ready) = self {
                    ready.history_go(false);
                }
                self.schedule_work()
            }
            Message::HistoryForward => {
                if let Session::Ready(ready) = self {
                    ready.history_go(true);
                }
                self.schedule_work()
            }
            Message::DocScrolled(y) => {
                if let Session::Ready(ready) = self {
                    ready.doc_scroll_y = y;
                    if ready.view_mode == ViewMode::Continuous {
                        let page = ready.page_at_offset(y);
                        if page != ready.visible {
                            ready.visible = page;
                            ready.sync_page_input();
                            ready.save_position();
                        }
                    }
                }
                self.schedule_work()
            }
            Message::SetViewMode(mode) => {
                let follow = if let Session::Ready(ready) = self {
                    ready.view_mode = mode;
                    ready.overflow_open = false;
                    ready.save_position();
                    nav_follow(ready)
                } else {
                    Task::none()
                };
                Task::batch([self.schedule_work(), follow])
            }
            Message::BrowseTo(path) => {
                if let Session::Empty(empty) = self {
                    empty.cwd = path.clone();
                    empty.listing_error = None;
                    return Task::perform(list_path(path.clone()), move |result| {
                        Message::ListingReady {
                            path: path.clone(),
                            result,
                        }
                    });
                }
                Task::none()
            }
            Message::ListingReady { path, result } => {
                if let Session::Empty(empty) = self {
                    if empty.cwd == path {
                        match result {
                            Ok(listing) => {
                                empty.listing = listing;
                                empty.listing_error = None;
                            }
                            Err(err) => {
                                empty.listing = Vec::new();
                                empty.listing_error = Some(err);
                            }
                        }
                    }
                }
                Task::none()
            }
            Message::RecentsReady(recents) => {
                match self {
                    Session::Empty(empty) => empty.recents = recents,
                    Session::Loading { recents: slot, .. } => *slot = recents,
                    Session::Ready(_) | Session::Failed { .. } => {}
                }
                Task::none()
            }
        }
    }

    pub fn view(&self) -> iced::Element<'_, Message> {
        crate::view::chrome(self, self.theme())
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        let events = event::listen_with(|event, status, id| match event {
            Event::Window(window::Event::FileDropped(path)) => Some(Message::FileDropped(path)),
            Event::Window(window::Event::Unfocused) => Some(Message::DragCancelled),
            Event::Window(window::Event::Opened { size, .. })
            | Event::Window(window::Event::Resized(size)) => Some(Message::WindowMetrics {
                width: size.width,
                // A faixa de abas encolhe o painel; quem desconta é
                // `WindowMetrics` (a altura da faixa é estado, não evento).
                height: (size.height - crate::view::CHROME_HEIGHT).max(1.0),
                id,
            }),
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                keyboard_message(key, modifiers, status)
            }
            _ => None,
        });
        // Tique só enquanto a primeira aba carrega: a barra indeterminada
        // (view.rs) precisa de repintura; parado o app não redesenha à toa.
        if matches!(self, Session::Loading { .. }) {
            iced::Subscription::batch([
                events,
                iced::time::every(LOADING_TICK).map(|_| Message::LoadingTick),
            ])
        } else {
            events
        }
    }

    pub fn begin_open(&mut self, source: OpenSource) -> Task<Message> {
        let recents = self.recents();
        let theme = self.theme();
        let render_scale = self.render_scale();
        let mut gen = self.open_gen().wrapping_add(1);
        if gen == 0 {
            gen = 1;
        }
        match self {
            // Com uma aba já aberta o documento entra em aba nova (issue #40):
            // a janela segue na aba atual, com página e zoom, até o arquivo
            // chegar. A `Loading` fica só para a primeira aba (vinda do `Empty`).
            Session::Ready(tabs) => {
                tabs.pending = Some((gen, source.clone()));
                tabs.open_error = None;
            }
            _ => {
                *self = Session::Loading {
                    source: source.clone(),
                    recents,
                    gen,
                    theme,
                    render_scale,
                    phase: 0,
                };
            }
        }
        Task::perform(open_ready(source), move |result| Message::Opened {
            gen,
            result,
        })
    }

    pub fn finish_open(&mut self, result: Result<Ready, OpenError>) {
        let Some(gen) = self.loading_gen() else {
            return;
        };
        self.apply_open(gen, result);
    }

    fn apply_open(&mut self, gen: u64, result: Result<Ready, OpenError>) {
        // Segunda aba (a janela já estava mostrando um documento) ou primeira?
        let new_tab = match self {
            Session::Loading { gen: current, .. } if *current == gen => false,
            Session::Ready(tabs) if tabs.pending_gen() == Some(gen) => true,
            _ => {
                // Documento que já saiu da tela (janela fechada no meio do
                // carregamento): solta o parse na worker antes de largar.
                if let Ok(ready) = &result {
                    ready.close_engine();
                }
                return;
            }
        };
        let recents = merge_recents(self.recents(), read_recents());
        let theme = self.theme();
        let render_scale = self.render_scale();
        match result {
            Ok(mut ready) => {
                let path = ready.source.path().to_path_buf();
                let recents = push_recent(recents, path);
                let _ = save_recents(&recents);
                ready.recents = recents;
                ready.theme = theme;
                ready.render_scale = render_scale;
                ready.open_gen = gen;
                ready.signatures_open = false;
                ready.pages_open = false;
                ready.outline_open = false;
                ready.outline = None;
                ready.outline_collapsed.clear();
                ready.outline_cursor = None;
                ready.outline_load_issued = false;
                ready.pages_scroll_y = 0.0;
                ready.overflow_open = false;
                ready.restore_position();
                ready.sync_page_input();
                ready.render_gen = 1;
                if new_tab {
                    // Aba nova: o documento que estava na tela continua inteiro.
                    let Session::Ready(tabs) = self else {
                        return;
                    };
                    tabs.push(ready);
                } else {
                    *self = Session::Ready(Tabs::single(ready));
                }
            }
            Err(err) => {
                if new_tab {
                    // A janela segue com as abas que já estavam abertas; a
                    // falha vira uma linha na faixa de abas.
                    if let Session::Ready(tabs) = self {
                        let name = tabs
                            .pending
                            .take()
                            .map(|(_, source)| source.path().display().to_string())
                            .unwrap_or_default();
                        tabs.open_error = Some(format!("Falha ao abrir {name}: {err}"));
                    }
                    return;
                }
                let source = match self {
                    Session::Loading { source, .. } => source.clone(),
                    Session::Failed { source, .. } => source.clone(),
                    Session::Ready(r) => r.source.clone(),
                    Session::Empty(_) => return,
                };
                let recents = match &err {
                    OpenError::Io(_) => drop_recent(recents, source.path()),
                    _ => recents,
                };
                let _ = save_recents(&recents);
                *self = Session::Failed {
                    source,
                    message: err.to_string(),
                    recents,
                    gen,
                    theme,
                    render_scale,
                };
            }
        }
    }

    fn close_document(&mut self) -> Task<Message> {
        let recents = self.recents();
        let theme = self.theme();
        let open_gen = self.open_gen();
        let render_scale = self.render_scale();
        // Toda aba solta o seu documento na worker (issue #40): sem isto o
        // parse de cada uma ficaria vivo até o processo acabar.
        if let Session::Ready(tabs) = self {
            for doc in tabs.docs() {
                doc.close_engine();
            }
        }
        *self = Session::Empty(EmptyState {
            recents,
            theme,
            open_gen,
            render_scale,
            ..EmptyState::default()
        });
        empty_tasks()
    }

    /// Fecha a aba `index` (⌘W ou o × da faixa). A última aba fecha a janela,
    /// como sempre: `Empty` com os recentes preservados.
    fn close_tab(&mut self, index: usize) -> Task<Message> {
        if self.modal_open() {
            return Task::none();
        }
        if matches!(self, Session::Ready(tabs) if tabs.len() == 1) {
            return self.close_document();
        }
        let Some(tabs) = self.tabs_mut() else {
            return Task::none();
        };
        if index >= tabs.len() {
            return Task::none();
        }
        tabs.remove(index).close_engine();
        Task::batch([self.schedule_work(), self.nav_follow_active()])
    }

    /// Troca a aba ativa (clique na faixa).
    fn select_tab(&mut self, index: usize) -> Task<Message> {
        if self.modal_open() {
            return Task::none();
        }
        let Some(tabs) = self.tabs_mut() else {
            return Task::none();
        };
        tabs.select(index);
        Task::batch([self.schedule_work(), self.nav_follow_active()])
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab dão a volta na faixa.
    fn cycle_tab(&mut self, step: i32) -> Task<Message> {
        if self.modal_open() {
            return Task::none();
        }
        let Some(tabs) = self.tabs_mut() else {
            return Task::none();
        };
        if tabs.len() < 2 {
            return Task::none();
        }
        tabs.cycle(step);
        Task::batch([self.schedule_work(), self.nav_follow_active()])
    }

    /// As abas abertas, quando há alguma.
    fn tabs_mut(&mut self) -> Option<&mut Tabs> {
        match self {
            Session::Ready(tabs) => Some(tabs),
            _ => None,
        }
    }

    /// Modal/overlay que captura a interação (impressão, nota, aviso de
    /// assinado): trocar ou fechar aba por baixo prenderia o estado na aba de
    /// origem — a resposta assíncrona voltaria para uma aba que saiu da tela.
    fn modal_open(&self) -> bool {
        matches!(self, Session::Ready(tabs)
            if tabs.print_dialog.is_some() || tabs.note_draft.is_some() || tabs.save_warning)
    }

    /// `nav_follow` do documento ativo (sem aba aberta é `Task::none`).
    fn nav_follow_active(&self) -> Task<Message> {
        match self {
            Session::Ready(tabs) => nav_follow(tabs),
            _ => Task::none(),
        }
    }

    /// Altura que a faixa de abas toma da janela (zero com um documento só).
    fn tab_strip_height(&self) -> f32 {
        match self {
            Session::Ready(tabs) => strip_height(tabs.len()),
            _ => 0.0,
        }
    }

    fn recents(&self) -> Vec<PathBuf> {
        match self {
            Session::Empty(empty) => empty.recents.clone(),
            Session::Loading { recents, .. } | Session::Failed { recents, .. } => recents.clone(),
            Session::Ready(ready) => ready.recents.clone(),
        }
    }
    pub fn theme(&self) -> Theme {
        match self {
            Session::Empty(empty) => empty.theme,
            Session::Loading { theme, .. } | Session::Failed { theme, .. } => *theme,
            Session::Ready(ready) => ready.theme,
        }
    }

    fn set_theme(&mut self, theme: Theme) {
        match self {
            Session::Empty(empty) => empty.theme = theme,
            Session::Loading { theme: t, .. } | Session::Failed { theme: t, .. } => *t = theme,
            Session::Ready(tabs) => tabs.set_theme(theme),
        }
    }

    fn close_overflow(&mut self) {
        if let Session::Ready(ready) = self {
            ready.overflow_open = false;
        }
    }
    fn render_scale(&self) -> f32 {
        match self {
            Session::Empty(empty) => empty.render_scale,
            Session::Loading { render_scale, .. } | Session::Failed { render_scale, .. } => {
                *render_scale
            }
            Session::Ready(ready) => ready.render_scale,
        }
    }

    fn set_render_scale(&mut self, scale: f32) {
        match self {
            Session::Empty(empty) => empty.render_scale = scale,
            Session::Loading {
                render_scale: s, ..
            }
            | Session::Failed {
                render_scale: s, ..
            } => *s = scale,
            Session::Ready(tabs) => tabs.set_render_scale(scale),
        }
    }

    fn open_gen(&self) -> u64 {
        match self {
            Session::Empty(empty) => empty.open_gen,
            Session::Loading { gen, .. } | Session::Failed { gen, .. } => *gen,
            // Maior geração de todas as abas: a próxima não colide com a de
            // uma aba que está fora da tela (as respostas casam por geração).
            Session::Ready(tabs) => tabs.max_gen(),
        }
    }

    /// Geração do documento que está carregando: a primeira aba (`Loading`) ou
    /// a aba nova pendente de uma janela já aberta (issue #40).
    fn loading_gen(&self) -> Option<u64> {
        match self {
            Session::Loading { gen, .. } => Some(*gen),
            Session::Ready(tabs) => tabs.pending_gen(),
            _ => None,
        }
    }

    /// Salto do sumário (clique ou Enter): mesmo caminho de `SetPage` — clamp +
    /// histórico —, depois reagenda o trabalho de render.
    fn outline_jump(&mut self, page: PageNo) -> Task<Message> {
        if let Session::Ready(ready) = self {
            ready.navigate_to(page);
        }
        self.schedule_work()
    }

    fn schedule_work(&mut self) -> Task<Message> {
        let Session::Ready(ready) = self else {
            return Task::none();
        };
        // Carregamento preguiçoso do outline: um por documento, fire-and-forget.
        if ready.outline.is_none() && !ready.outline_load_issued {
            ready.outline_load_issued = true;
            let doc_gen = ready.open_gen;
            return outline_task(ready.engine.clone(), doc_gen);
        }
        if !ready.page_data_inflight.is_empty() || !ready.render_inflight.is_empty() {
            return Task::none();
        }
        ready.evict_unused();
        if let Some(task) = ready.request_visible_render() {
            return task;
        }
        if let Some(page) = ready.next_page_data_target() {
            let doc_gen = ready.open_gen;
            ready.page_data_inflight.insert(page.index());
            return page_data_task(ready.engine.clone(), page, doc_gen);
        }
        if let Some(task) = ready.request_speculative_render() {
            return task;
        }
        ready.request_thumb_render()
    }
}

fn empty_tasks() -> Task<Message> {
    Task::batch([
        Task::perform(load_recents(), Message::RecentsReady),
        Task::perform(list_path(None), |result| Message::ListingReady {
            path: None,
            result,
        }),
    ])
}

fn page_data_task(engine: PdfiumEngine, page: PageNo, doc_gen: u64) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || engine.page_data(page).map_err(|e| e.to_string()))
                .await
                .map_err(|e| e.to_string())?
        },
        move |result| Message::PageData {
            page,
            doc_gen,
            result,
        },
    )
}

/// Carrega o outline (bookmarks) do documento em background. Read-only sobre
/// o Pdfium; resulta em `Message::OutlineLoaded`.
fn outline_task(engine: PdfiumEngine, doc_gen: u64) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || engine.outline())
                .await
                .ok()
                .and_then(|result| result.ok())
                .flatten()
        },
        move |outline| Message::OutlineLoaded { doc_gen, outline },
    )
}

fn render_task(
    engine: PdfiumEngine,
    page: PageNo,
    scale: Scale,
    rotation: u8,
    doc_gen: u64,
    render_gen: u64,
) -> Task<Message> {
    Task::perform(
        async move {
            match tokio::task::spawn_blocking(move || engine.render(page, scale, rotation)).await {
                Ok(Ok(surface)) => (page, scale, rotation, Some(surface)),
                _ => (page, scale, rotation, None),
            }
        },
        move |(page, scale, rotation, surface)| Message::Rendered {
            page,
            scale,
            rotation,
            doc_gen,
            render_gen,
            surface,
        },
    )
}

/// Em rolagem contínua, toda troca/zoom/resize rola o painel até `visible`.
/// Nos demais modos (e sem documento) é `Task::none`.
fn nav_follow(ready: &Ready) -> Task<Message> {
    if ready.view_mode != ViewMode::Continuous || ready.pages.total == 0 {
        return Task::none();
    }
    scrollable::scroll_to(
        crate::view::doc_scroll_id(),
        scrollable::AbsoluteOffset {
            x: 0.0,
            y: ready.page_offset(ready.visible),
        },
    )
}

pub(crate) fn keyboard_message(
    key: Key,
    modifiers: keyboard::Modifiers,
    status: event::Status,
) -> Option<Message> {
    // ⌘/Ctrl+Enter salva o post-it antes da guarda de foco: com o editor
    // focado o campo captura a tecla (status Captured) e a guarda a mataria.
    // Sem rascunho aberto o handler descarta (no-op).
    if (modifiers.logo() || modifiers.control()) && !modifiers.alt() {
        if let Key::Named(Named::Enter) = key.as_ref() {
            return Some(Message::NoteSave);
        }
    }
    if status != event::Status::Ignored {
        return None;
    }
    // Ctrl/Cmd+Z desfaz, com Shift refaz (antes do hist_mod: ⌘/Ctrl engolem
    // o resto das teclas no bloco abaixo; guarda de foco cobre inputs).
    #[cfg(target_os = "macos")]
    let cmd = modifiers.logo() && !modifiers.control() && !modifiers.alt();
    #[cfg(not(target_os = "macos"))]
    let cmd = modifiers.control() && !modifiers.logo() && !modifiers.alt();
    if cmd {
        match key.as_ref() {
            Key::Character("z" | "Z") if modifiers.shift() => return Some(Message::AnnotRedo),
            Key::Character("z" | "Z") => return Some(Message::AnnotUndo),
            // Abrir entra em aba nova quando já há documento (issue #40).
            Key::Character("t" | "T") => return Some(Message::PickFile),
            Key::Character("w" | "W") => return Some(Message::CloseTabActive),
            _ => {}
        }
    }
    // Ctrl+Tab / Ctrl+Shift+Tab alternam abas (nas duas plataformas; ⌘⇥ é do
    // sistema no macOS). Antes da guarda de modificadores abaixo.
    if modifiers.control() && !modifiers.logo() && !modifiers.alt() {
        if let Key::Named(Named::Tab) = key.as_ref() {
            return Some(Message::CycleTab(if modifiers.shift() { -1 } else { 1 }));
        }
    }
    // Alt+←/→ (⌘ no mac): histórico voltar/avançar.
    #[cfg(target_os = "macos")]
    let hist_mod =
        modifiers.logo() && !modifiers.alt() && !modifiers.control() && !modifiers.shift();
    #[cfg(not(target_os = "macos"))]
    let hist_mod =
        modifiers.alt() && !modifiers.logo() && !modifiers.control() && !modifiers.shift();
    if hist_mod {
        return match key.as_ref() {
            Key::Named(Named::ArrowLeft) => Some(Message::HistoryBack),
            Key::Named(Named::ArrowRight) => Some(Message::HistoryForward),
            _ => None,
        };
    }
    if modifiers.shift() || modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    match key.as_ref() {
        Key::Named(Named::PageUp | Named::ArrowLeft) => Some(Message::Nav(NavCmd::Previous)),
        Key::Named(Named::PageDown | Named::ArrowRight) => Some(Message::Nav(NavCmd::Next)),
        Key::Named(Named::Home) => Some(Message::Nav(NavCmd::First)),
        Key::Named(Named::End) => Some(Message::Nav(NavCmd::Last)),
        // R gira a vista; com foco em campo o iced captura antes (guarda de foco).
        Key::Character("r" | "R") => Some(Message::RotateView),
        // H/U/S marcam a seleção (o handler ignora sem seleção com texto).
        Key::Character("h" | "H") => Some(Message::Annotate(AnnotKind::Highlight)),
        Key::Character("u" | "U") => Some(Message::Annotate(AnnotKind::Underline)),
        Key::Character("s" | "S") => Some(Message::Annotate(AnnotKind::Strikeout)),
        // N abre o rascunho de nota da seleção (handler ignora sem seleção
        // com texto; com draft já aberto é no-op para não apagar o digitado).
        Key::Character("n" | "N") => Some(Message::Annotate(AnnotKind::Note)),
        // Sem diálogo aberto o handler ignora; com foco em campo, o iced captura antes.
        Key::Named(Named::Escape) => Some(Message::ClosePrintDialog),
        // ↑/↓ e Enter andam/saltam na árvore do sumário; sem a aba aberta o
        // handler ignora (setas horizontais continuam com o histórico e a nav).
        Key::Named(Named::ArrowUp) => Some(Message::OutlineKey(OutlineKey::Prev)),
        Key::Named(Named::ArrowDown) => Some(Message::OutlineKey(OutlineKey::Next)),
        Key::Named(Named::Enter) => Some(Message::OutlineKey(OutlineKey::Activate)),
        _ => None,
    }
}

/// Pergunta o DPR da janela ao backend (precisa do `id` do evento).
fn query_window_scale(id: window::Id) -> Task<Message> {
    window::get_scale_factor(id).map(Message::WindowScale)
}

pub fn thumbnail_scale(media: MediaBox) -> Scale {
    Scale::from_factor((THUMB_WIDTH / media.width.max(1.0)).clamp(0.05, 2.0))
}

impl Ready {
    pub fn page_count(&self) -> u32 {
        self.pages.total
    }

    /// Solta o documento na worker do motor (aba fechada, issue #40). Os
    /// clones deste `Ready` compartilham o mesmo documento — depois disto os
    /// pedidos deles falham.
    pub(crate) fn close_engine(&self) {
        self.engine.close();
    }

    pub fn media(&self, page: PageNo) -> MediaBox {
        self.loaded_media(page).unwrap_or(MediaBox {
            width: 1.0,
            height: 1.0,
        })
    }

    /// Escala de render em px físicos: zoom CSS × DPR da janela.
    /// Guarda <1.0 (campo ainda desconhecido) como 1.0.
    /// Ajuste usa a mídia girada: 90°/270° trocam largura ↔ altura.
    fn page_scale(&self, page: PageNo) -> Scale {
        let css = self.sheet_css(page);
        let dpr = if self.render_scale >= 1.0 {
            self.render_scale
        } else {
            1.0
        };
        Scale::from_factor(css * dpr)
    }

    /// Fator de zoom exibido (CSS, 1.0 = 100%) de onde partem os passos de
    /// +/− da barra. Ajuste na mídia girada, como o render: com a vista a
    /// 90°/270° o passo parte do que está na tela, não da página original.
    pub(crate) fn zoom_step_factor(&self) -> f32 {
        self.sheet_css(self.visible)
    }

    fn thumb_scale_for(&self, media: MediaBox) -> Scale {
        let dpr = if self.render_scale >= 1.0 {
            self.render_scale
        } else {
            1.0
        };
        Scale::from_factor(thumbnail_scale(media).factor() * dpr)
    }

    fn loaded_media(&self, page: PageNo) -> Option<MediaBox> {
        self.pages.media.get(page.index() as usize).and_then(|m| *m)
    }

    /// Mídia na orientação da vista: rotação ímpar troca largura ↔ altura.
    pub(crate) fn rotated_media(&self, page: PageNo) -> MediaBox {
        let media = self.media(page);
        if self.view_rotation & 1 == 1 {
            MediaBox {
                width: media.height,
                height: media.width,
            }
        } else {
            media
        }
    }

    fn has_page_data(&self, page: PageNo) -> bool {
        self.loaded_media(page).is_some()
            && matches!(self.pages.text.get(page.index() as usize), Some(Some(_)))
    }

    pub fn page_input(&self) -> &str {
        &self.page_input
    }

    pub(crate) fn surface(&self, page: PageNo, scale: Scale) -> Option<&CachedSurface> {
        self.surfaces.get(page, scale, self.view_rotation)
    }

    pub(crate) fn thumb_surface(&self, page: PageNo) -> Option<&CachedSurface> {
        let media = self.loaded_media(page)?;
        self.thumbs.get(page, self.thumb_scale_for(media))
    }

    /// Página atual do preview de impressão (`None` sem diálogo aberto).
    fn print_preview_target(&self) -> Option<PageNo> {
        let dialog = self.print_dialog.as_ref()?;
        let pages = dialog.preview_pages(self.pages.total, self.visible);
        pages
            .get(dialog.preview.min(pages.len().saturating_sub(1)))
            .copied()
    }

    pub(crate) fn visible_surface(&self) -> Option<&CachedSurface> {
        self.page_surface(self.visible)
    }

    /// Bitmap da página na escala atual, com stale-while-revalidate.
    pub(crate) fn page_surface(&self, page: PageNo) -> Option<&CachedSurface> {
        let scale = self.page_scale(page);
        self.surface(page, scale)
            .or_else(|| self.surfaces.fallback_for_page(page))
    }

    /// Largura útil do documento: espelha `page_pane`/`ready_body` (view.rs).
    /// É a referência do ajuste à largura — a folha desenhada (`sheet_width`)
    /// parte daqui e aplica o zoom.
    pub(crate) fn doc_content_width(&self) -> f32 {
        let mut w = self.viewport.width - CHROME_PAD;
        if self.pages_open {
            w -= PAGES_PANEL_W + PANES_GAP;
        }
        if self.signatures_open {
            w -= SIG_PANEL_W + PANES_GAP;
        }
        (w - 2.0 * DOC_PAD_X).max(1.0)
    }

    /// Altura útil do painel: a janela menos o chrome menos o respiro da
    /// moldura (`CHROME_PAD`, espelha `chrome()` em view.rs). A faixa de abas
    /// já vem descontada no `WindowMetrics`.
    pub(crate) fn doc_content_height(&self) -> f32 {
        (self.viewport.height - CHROME_PAD).max(1.0)
    }

    /// Fator CSS do zoom (1.0 = 100%, 1pt = 1px) na página: ajuste à largura
    /// usa a largura útil, ajuste à página cabe na moldura, manual é absoluto.
    /// Mesma base do render (`page_scale`), do desenho (`sheet_size`) e do
    /// rótulo (`zoom_step_factor`) — os quatro andam juntos, senão a folha,
    /// o bitmap e o % divergem.
    pub(crate) fn sheet_css(&self, page: PageNo) -> f32 {
        let media = self.rotated_media(page);
        match self.zoom {
            Zoom::Manual(z) => z.get(),
            Zoom::Width => self.doc_content_width() / media.width.max(1.0),
            Zoom::Page => (self.doc_content_width() / media.width.max(1.0))
                .min(self.doc_content_height() / media.height.max(1.0)),
        }
    }

    /// Largura da folha na janela (px CSS): mídia girada × `sheet_css`. É o
    /// que a vista desenha — em `Zoom::Width` coincide com a largura útil.
    pub(crate) fn sheet_width(&self, page: PageNo) -> f32 {
        (self.rotated_media(page).width.max(1.0) * self.sheet_css(page)).max(1.0)
    }

    /// Largura do palco rolável na página: a moldura útil ou a folha +
    /// respiro, o que for maior. Conteúdo direto do `scrollable` não pode ser
    /// `Fill` no eixo de rolagem (o iced dá assert), então o palco mede aqui:
    /// folha estreita centraliza no palco cheio, folha larga rola na
    /// horizontal. O −1px evita barra horizontal por erro de float (o fundo é
    /// a mesma cor, invisível).
    pub(crate) fn doc_stage_width(&self, page: PageNo) -> f32 {
        let pane = self.doc_content_width() + 2.0 * DOC_PAD_X;
        (pane - 1.0).max(self.sheet_width(page) + 2.0 * DOC_PAD_X)
    }

    /// Palco do modo contínuo: cobre a página mais larga (páginas mistas).
    pub(crate) fn doc_stage_max_width(&self) -> f32 {
        let mut w = self.doc_content_width() + 2.0 * DOC_PAD_X - 1.0;
        for i in 0..self.pages.total {
            let p = PageNo::from_index(i);
            w = w.max(self.sheet_width(p) + 2.0 * DOC_PAD_X);
        }
        w.max(1.0)
    }

    /// Tamanho da folha na janela (px CSS): largura da folha × proporção da
    /// mídia girada — o mesmo par que a vista usa para desenhar (`with_marks`).
    pub(crate) fn sheet_size(&self, page: PageNo) -> [f32; 2] {
        let sw = self.sheet_width(page);
        let rotated = self.rotated_media(page);
        [sw, sw * rotated.height.max(1.0) / rotated.width.max(1.0)]
    }

    /// Altura da célula (padding + folha proporcional à mídia girada).
    pub(crate) fn doc_cell_height(&self, page: PageNo, sheet_width: f32) -> f32 {
        let media = self.rotated_media(page);
        DOC_PAD_TOP + sheet_width * media.height.max(1.0) / media.width.max(1.0) + DOC_PAD_BOTTOM
    }

    /// Offset Y do topo da página na coluna contínua.
    pub(crate) fn page_offset(&self, page: PageNo) -> f32 {
        let mut y = 0.0;
        for i in 0..page.index().min(self.pages.total) {
            let p = PageNo::from_index(i);
            y += self.doc_cell_height(p, self.sheet_width(p)) + DOC_GAP;
        }
        y
    }

    /// Primeira página cujo intervalo contém `y` (clamp no fim).
    pub(crate) fn page_at_offset(&self, y: f32) -> PageNo {
        if self.pages.total == 0 {
            return PageNo::first();
        }
        let mut top = 0.0;
        for i in 0..self.pages.total {
            let p = PageNo::from_index(i);
            top += self.doc_cell_height(p, self.sheet_width(p));
            if y < top {
                return PageNo::from_index(i);
            }
            top += DOC_GAP;
        }
        PageNo::from_index(self.pages.total - 1)
    }

    pub(crate) fn doc_total_height(&self) -> f32 {
        if self.pages.total == 0 {
            return 0.0;
        }
        let last = PageNo::from_index(self.pages.total - 1);
        self.page_offset(last) + self.doc_cell_height(last, self.sheet_width(last))
    }

    /// Janela com widget montado: visíveis na viewport estimada ± 2 páginas.
    pub(crate) fn doc_window(&self) -> (u32, u32) {
        if self.pages.total == 0 {
            return (0, 0);
        }
        let pane_h = self.viewport.height.max(1.0);
        let lo = (self.doc_scroll_y - pane_h).max(0.0);
        let hi = self.doc_scroll_y + pane_h * 2.0;
        let start = self.page_at_offset(lo).index().saturating_sub(2);
        let end = (self.page_at_offset(hi).index() + 3).min(self.pages.total);
        (start, end)
    }

    /// Páginas retidas no cache: janela do contínuo ou `visible ± 1`.
    fn keep_pages(&self) -> HashSet<u32> {
        if self.view_mode == ViewMode::Continuous {
            let (start, end) = self.doc_window();
            (start..end).collect()
        } else {
            neighbor_page_set(self.visible.index(), self.pages.total)
        }
    }

    pub fn visible_render_failed(&self) -> bool {
        let scale = self.page_scale(self.visible);
        self.failed
            .contains(&render_key(self.visible, scale, self.view_rotation))
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn text_layers(&self) -> Vec<&TextLayer> {
        self.pages.text.iter().flatten().collect()
    }

    pub fn recents(&self) -> &[PathBuf] {
        &self.recents
    }

    pub fn selection_plain_text(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        let layer = self.pages.text.get(sel.page.index() as usize)?.as_ref()?;
        let sliced = layer.slice(sel.range);
        if sliced.is_empty() {
            None
        } else {
            Some(sliced)
        }
    }

    /// Cria marcação da seleção atual; `false` sem seleção com texto (ignora).
    pub(crate) fn annotate_selection(&mut self, kind: AnnotKind) -> bool {
        let Some(sel) = self.selection.clone() else {
            return false;
        };
        let quads = match self
            .pages
            .text
            .get(sel.page.index() as usize)
            .and_then(|l| l.as_ref())
        {
            Some(layer) if !layer.slice(sel.range).trim().is_empty() => {
                quads_for_range_by_line(&layer.glyphs, sel.range.start, sel.range.end)
            }
            _ => return false,
        };
        if quads.is_empty() {
            return false;
        }
        let annot = Annotation {
            id: self.next_annot_id,
            page: sel.page,
            range: sel.range,
            quads,
            kind,
            // H/U/S não carregam texto; apenas notas (`save_note_draft`).
            text: String::new(),
            // Só nota tem marcador (e ele nasce no trecho: `marker: None`).
            marker: None,
        };
        self.next_annot_id += 1;
        self.apply_annot_action(AnnotAction::Add(annot.clone()));
        self.annot_undo.push(AnnotAction::Add(annot));
        self.annot_redo.clear();
        true
    }

    /// Abre o rascunho de nota da seleção atual (N / menu ⋯); `false` sem
    /// seleção com texto, ou com draft já aberto (no-op — não apaga o texto
    /// digitado). Sobre o trecho de uma nota existente, reabre em modo de
    /// edição com o texto dela.
    pub(crate) fn open_note_draft(&mut self) -> bool {
        if self.note_draft.is_some() {
            return false;
        }
        let Some(sel) = self.selection.clone() else {
            return false;
        };
        let quads = match self
            .pages
            .text
            .get(sel.page.index() as usize)
            .and_then(|l| l.as_ref())
        {
            Some(layer) if !layer.slice(sel.range).trim().is_empty() => {
                quads_for_range_by_line(&layer.glyphs, sel.range.start, sel.range.end)
            }
            _ => return false,
        };
        if quads.is_empty() {
            return false;
        }
        let existing = self
            .annotations
            .iter()
            .find(|a| a.kind == AnnotKind::Note && a.page == sel.page && a.range == sel.range);
        let (editing, text, marker) = match existing {
            Some(a) => (Some(a.id), a.text.clone(), a.marker_pt()),
            None => (None, String::new(), derived_marker_pt(&quads)),
        };
        let anchor = self.note_anchor(sel.page, &quads, marker);
        self.note_draft = Some(NoteDraft {
            page: sel.page,
            range: sel.range,
            quads,
            content: text_editor::Content::with_text(&text),
            editing,
            anchor,
            anchor_scroll: self.doc_scroll_y,
        });
        true
    }

    /// Canto do post-it na janela (px CSS, Y para baixo) para uma nota: canto
    /// do marcador na folha (mesma matemática do canvas: `display_pt`) somado
    /// à origem da folha capturada no último press, descontada a rolagem desde
    /// então. Sem clique ainda, a folha está em (0, 0).
    fn note_anchor(&self, page: PageNo, quads: &[Quad], marker_pt: [f32; 2]) -> [f32; 2] {
        let [cw, ch] = self.sheet_size(page);
        let media = self.media(page);
        let rotation = self.view_rotation;
        let [mx, my] = display_pt(marker_pt, media, rotation, cw, ch);
        // Marcador compacto + respiro: o post-it nasce ao lado dele.
        let side = marker_side(quads, media, rotation, cw, ch);
        let scrolled = self.doc_scroll_y - self.sheet_scroll;
        [
            self.sheet_at[0] + mx + side + POSTIT_GAP,
            self.sheet_at[1] + my - scrolled,
        ]
    }

    /// Posição do post-it na janela: a âncora do rascunho menos a rolagem
    /// desde que abriu, presa à janela (nunca sai da tela) com o tamanho
    /// dado para não cobrir os botões.
    pub(crate) fn postit_pos(&self, size: [f32; 2]) -> [f32; 2] {
        let Some(draft) = self.note_draft.as_ref() else {
            return [POSTIT_MARGIN, POSTIT_MARGIN];
        };
        let pos = [
            draft.anchor[0],
            draft.anchor[1] - (self.doc_scroll_y - draft.anchor_scroll),
        ];
        clamp_postit(pos, size, [self.viewport.width, self.viewport.height])
    }

    /// Abre a edição de uma nota existente (clique sobre o marcador): ancora
    /// o draft no trecho dela com o texto atual, sem tocar na seleção (o
    /// trecho sublinhado não "vem junto"). `false` se não é nota ou com
    /// draft já aberto (o clique então não remove a nota — vira no-op).
    fn open_note_draft_for_id(&mut self, id: u64) -> bool {
        let Some(annot) = self
            .annotations
            .iter()
            .find(|a| a.id == id && a.kind == AnnotKind::Note)
            .cloned()
        else {
            return false;
        };
        if self.note_draft.is_some() {
            return false;
        }
        let anchor = self.note_anchor(annot.page, &annot.quads, annot.marker_pt());
        self.note_draft = Some(NoteDraft {
            page: annot.page,
            range: annot.range,
            quads: annot.quads.clone(),
            content: text_editor::Content::with_text(&annot.text),
            editing: Some(annot.id),
            anchor,
            anchor_scroll: self.doc_scroll_y,
        });
        true
    }

    /// Salva o rascunho como nota (`kind: Note`, texto do draft) via pilha
    /// de undo (limpa o redo, como `annotate_selection`). Texto vazio ou
    /// só-espaço descarta sem criar (sem undo entry). Edição de nota
    /// existente vira `Remove(antiga)` + `Add(nova)` na pilha — desfazer os
    /// dois passos restaura o texto antigo. `false` sem draft ou descartado.
    pub(crate) fn save_note_draft(&mut self) -> bool {
        let Some(draft) = self.note_draft.take() else {
            return false;
        };
        // O editor do iced sempre fecha a última linha com `\n`: guarda o
        // texto como digitado (sem a quebra que o widget acrescenta).
        let text = draft.content.text();
        if text.trim().is_empty() {
            return false;
        }
        let text = text.trim_end().to_string();
        // Edição substitui a nota (Remove + Add): o marcador vai junto, senão
        // reescrever o texto devolveria o ícone amarelo para o trecho.
        let previous = draft
            .editing
            .and_then(|id| self.annotations.iter().find(|a| a.id == id).cloned());
        if let Some(old) = previous.as_ref() {
            self.apply_annot_action(AnnotAction::Remove(old.clone()));
            self.annot_undo.push(AnnotAction::Remove(old.clone()));
        }
        let annot = Annotation {
            id: self.next_annot_id,
            page: draft.page,
            range: draft.range,
            quads: draft.quads,
            kind: AnnotKind::Note,
            text,
            marker: previous.and_then(|old| old.marker),
        };
        self.next_annot_id += 1;
        self.apply_annot_action(AnnotAction::Add(annot.clone()));
        self.annot_undo.push(AnnotAction::Add(annot));
        self.annot_redo.clear();
        true
    }

    /// Botão vermelho do post-it: remove a nota em edição e fecha o rascunho.
    /// Só vale em modo de edição (`editing: Some(id)`) — criando, o botão nem
    /// existe; a remoção entra na pilha de undo (`Remove`, como o clique).
    pub(crate) fn delete_draft_note(&mut self) -> bool {
        let Some(id) = self.note_draft.as_ref().and_then(|draft| draft.editing) else {
            return false;
        };
        if !self.remove_annotation(id) {
            return false;
        }
        self.note_draft = None;
        true
    }

    /// Remove por id (clique sobre nota abre edição, não remove); `false` se não existe.
    pub(crate) fn remove_annotation(&mut self, id: u64) -> bool {
        let Some(annot) = self.annotations.iter().find(|a| a.id == id).cloned() else {
            return false;
        };
        self.apply_annot_action(AnnotAction::Remove(annot.clone()));
        self.annot_undo.push(AnnotAction::Remove(annot));
        self.annot_redo.clear();
        true
    }

    /// Press sobre o marcador de uma nota: começa um arrasto candidato (delta
    /// zero) e larga a seleção — o arrasto move o ícone, não estende texto.
    /// Só o marcador inicia (`marker_hit`, que acompanha o arrasto); o press
    /// no trecho sublinhado segue o fluxo de seleção normal. `false` = o
    /// press não é sobre marcador; sem arrasto, o `PointerUp` abre a edição.
    fn begin_note_drag(&mut self, page: PageNo, page_pt: [f32; 2]) -> bool {
        let note = self
            .annotations
            .iter()
            .find(|a| a.kind == AnnotKind::Note && a.page == page && self.marker_hit(a, page_pt))
            .cloned();
        let Some(note) = note else {
            return false;
        };
        self.selection = None;
        self.press_anchor = None;
        self.note_drag = Some(NoteDrag {
            id: note.id,
            page,
            from: page_pt,
            marker0: note.marker_pt(),
            delta: [0.0, 0.0],
        });
        true
    }

    /// Atualiza o delta do arrasto em curso (mover é dentro de uma página só:
    /// pontos de outra página não mexem no candidato). `true` = o movimento
    /// era do arrasto de nota.
    fn update_note_drag(&mut self, page: PageNo, page_pt: [f32; 2]) -> bool {
        let Some(drag) = self.note_drag.as_mut().filter(|drag| drag.page == page) else {
            return false;
        };
        drag.delta = [page_pt[0] - drag.from[0], page_pt[1] - drag.from[1]];
        true
    }

    /// Solta o press: arrasto além do limiar move o marcador (preso à
    /// página); abaixo dele é o clique que abre a edição. `true` = o post-it
    /// abriu (a vista então o foca).
    fn finish_note_drag(&mut self, page: PageNo, page_pt: [f32; 2]) -> bool {
        if self.note_drag.is_none() {
            return false;
        }
        self.update_note_drag(page, page_pt);
        let Some(drag) = self.note_drag.take() else {
            return false;
        };
        if self.drag_px(&drag) < NOTE_DRAG_MIN_PX {
            return self.open_note_draft_for_id(drag.id);
        }
        self.move_note_marker(&drag);
        false
    }

    /// Delta do arrasto em px CSS na escala atual (limiar clique-vs-arrasto):
    /// pontos de página → px acompanham o zoom da folha.
    fn drag_px(&self, drag: &NoteDrag) -> f32 {
        let scale = self.doc_content_width() / self.rotated_media(drag.page).width.max(1.0);
        let (dx, dy) = (drag.delta[0] * scale, drag.delta[1] * scale);
        (dx * dx + dy * dy).sqrt()
    }

    /// Solta o marcador onde o ghost indicava: clamp na página (sem snap em
    /// glifo — o resultado é o ghost que estava na tela) e `marker: Some(...)`
    /// com id, trecho, quads e texto intactos. Entra na pilha como `Remove` +
    /// `Add`, como o save da edição. `false` se o marcador não saiu do lugar.
    fn move_note_marker(&mut self, drag: &NoteDrag) -> bool {
        let to = drag.candidate(self.media(drag.page));
        let Some(old) = self
            .annotations
            .iter()
            .find(|a| a.id == drag.id && a.kind == AnnotKind::Note)
            .cloned()
        else {
            return false;
        };
        if old.marker_pt() == to {
            return false;
        }
        let moved = Annotation {
            marker: Some(to),
            ..old.clone()
        };
        self.apply_annot_action(AnnotAction::Remove(old.clone()));
        self.annot_undo.push(AnnotAction::Remove(old.clone()));
        self.apply_annot_action(AnnotAction::Add(moved.clone()));
        self.annot_undo.push(AnnotAction::Add(moved));
        self.annot_redo.clear();
        true
    }

    /// Marcador candidato do arrasto nesta página (ghost pontilhado do
    /// canvas): `(id da nota, canto em pontos de página)` — o mesmo que o
    /// soltar aplica. `None` = sem arrasto, ou arrasto abaixo do limiar.
    pub(crate) fn note_drag_ghost(&self, page: PageNo) -> Option<(u64, [f32; 2])> {
        let drag = self.note_drag.as_ref().filter(|drag| drag.page == page)?;
        if self.drag_px(drag) < NOTE_DRAG_MIN_PX {
            return None;
        }
        Some((drag.id, drag.candidate(self.media(page))))
    }

    pub(crate) fn annot_undo_once(&mut self) -> bool {
        let Some(action) = self.annot_undo.pop() else {
            return false;
        };
        let inverse = match &action {
            AnnotAction::Add(a) => AnnotAction::Remove(a.clone()),
            AnnotAction::Remove(a) => AnnotAction::Add(a.clone()),
        };
        self.apply_annot_action(inverse);
        self.annot_redo.push(action);
        true
    }

    pub(crate) fn annot_redo_once(&mut self) -> bool {
        let Some(action) = self.annot_redo.pop() else {
            return false;
        };
        self.apply_annot_action(action.clone());
        self.annot_undo.push(action);
        true
    }

    pub(crate) fn can_annot_undo(&self) -> bool {
        !self.annot_undo.is_empty()
    }

    pub(crate) fn can_annot_redo(&self) -> bool {
        !self.annot_redo.is_empty()
    }

    /// Id da marcação sob o ponto (espaço da mídia original); `None` fora.
    /// Nota acerta só no marcador (`marker_hit`, que acompanha o arrasto); o
    /// trecho sublinhado é só âncora visual — o press/clique nele seleciona
    /// texto em vez de abrir/arrastar a nota.
    pub(crate) fn annotation_at(&self, page: PageNo, page_pt: [f32; 2]) -> Option<u64> {
        self.annotations
            .iter()
            .filter(|a| a.page == page)
            .find(|a| match a.kind {
                AnnotKind::Note => self.marker_hit(a, page_pt),
                _ => {
                    let [x, y] = page_pt;
                    a.quads.iter().any(|q| q.contains(x, y))
                }
            })
            .map(|a| a.id)
    }

    /// O ponto cai no quadrado do marcador de uma nota? A conta é a do
    /// desenho (`display_pt`/`marker_side`, em px da folha), então o clique
    /// acerta o ícone mesmo arrastado para longe do trecho.
    fn marker_hit(&self, annot: &Annotation, page_pt: [f32; 2]) -> bool {
        if annot.kind != AnnotKind::Note {
            return false;
        }
        let page = annot.page;
        let [cw, ch] = self.sheet_size(page);
        let media = self.media(page);
        let rotation = self.view_rotation;
        let [px, py] = display_pt(page_pt, media, rotation, cw, ch);
        let [mx, my] = display_pt(annot.marker_pt(), media, rotation, cw, ch);
        let side = marker_side(&annot.quads, media, rotation, cw, ch);
        px >= mx - NOTE_MARKER_HIT_PAD
            && px <= mx + side + NOTE_MARKER_HIT_PAD
            && py >= my - NOTE_MARKER_HIT_PAD
            && py <= my + side + NOTE_MARKER_HIT_PAD
    }

    fn apply_annot_action(&mut self, action: AnnotAction) {
        match action {
            AnnotAction::Add(a) => {
                if self.annotations.iter().all(|e| e.id != a.id) {
                    self.annotations.push(a);
                }
            }
            AnnotAction::Remove(a) => self.annotations.retain(|e| e.id != a.id),
        }
    }

    /// Retângulos da seleção atual em espaço da mídia original, um por linha
    /// (`None` sem texto).
    pub(crate) fn selection_quads(&self) -> Option<(PageNo, Vec<Quad>)> {
        let sel = self.selection.as_ref()?;
        let layer = self.pages.text.get(sel.page.index() as usize)?.as_ref()?;
        if layer.slice(sel.range).trim().is_empty() {
            return None;
        }
        let quads = quads_for_range_by_line(&layer.glyphs, sel.range.start, sel.range.end);
        if quads.is_empty() {
            return None;
        }
        Some((sel.page, quads))
    }

    pub fn set_query(&mut self, query: String) {
        self.search = Search::derive(&query, &self.pages.text);
    }

    /// Linhas achatadas da árvore para a view: (caminho, profundidade, título,
    /// página, tem_filhos). Respeita `outline_collapsed`; vazia sem outline.
    pub(crate) fn outline_rows(&self) -> Vec<(Vec<usize>, usize, &str, PageNo, bool)> {
        let Some(outline) = &self.outline else {
            return Vec::new();
        };
        fn walk<'a>(
            items: &'a [OutlineItem],
            depth: usize,
            path: &mut Vec<usize>,
            collapsed: &HashSet<Vec<usize>>,
            rows: &mut Vec<(Vec<usize>, usize, &'a str, PageNo, bool)>,
        ) {
            for (i, item) in items.iter().enumerate() {
                path.push(i);
                rows.push((
                    path.clone(),
                    depth,
                    item.title.as_str(),
                    item.page,
                    !item.children.is_empty(),
                ));
                if !collapsed.contains(path) {
                    walk(&item.children, depth + 1, path, collapsed, rows);
                }
                path.pop();
            }
        }
        let mut rows = Vec::new();
        walk(
            &outline.items,
            0,
            &mut Vec::new(),
            &self.outline_collapsed,
            &mut rows,
        );
        rows
    }

    /// Caminho do último item com página <= `visible` (semântica de intervalo).
    pub(crate) fn outline_active(&self) -> Option<Vec<usize>> {
        self.outline_rows()
            .into_iter()
            .filter(|(_, _, _, page, _)| page.index() <= self.visible.index())
            .map(|(path, _, _, _, _)| path)
            .last()
    }

    /// Linha sob o cursor do teclado: o movimento explícito (↑/↓) ou, sem
    /// movimento, a entrada ativa — começar a andar de onde a página está.
    pub(crate) fn outline_focus(&self) -> Option<Vec<usize>> {
        self.outline_cursor
            .clone()
            .or_else(|| self.outline_active())
    }

    /// Anda uma linha (↑/↓) sem sair da lista e devolve o índice da nova linha,
    /// que a view rola para manter o cursor visível.
    fn outline_step(&mut self, next: bool) -> Option<usize> {
        let rows = self.outline_rows();
        if rows.is_empty() {
            return None;
        }
        let index = self
            .outline_focus()
            .and_then(|path| rows.iter().position(|(row, ..)| *row == path));
        let target = match (index, next) {
            (Some(i), true) => (i + 1).min(rows.len() - 1),
            (Some(i), false) => i.saturating_sub(1),
            (None, true) => 0,
            (None, false) => rows.len() - 1,
        };
        self.outline_cursor = Some(rows[target].0.clone());
        Some(target)
    }

    /// Página da linha sob o cursor (Enter); `None` sem cursor válido.
    fn outline_cursor_page(&self) -> Option<PageNo> {
        let path = self.outline_focus()?;
        self.outline_rows()
            .into_iter()
            .find(|(row, ..)| *row == path)
            .map(|(_, _, _, page, _)| page)
    }

    pub(crate) fn thumb_page_window(&self) -> Vec<PageNo> {
        let first = (self.pages_scroll_y / THUMB_ROW).floor().max(0.0) as u32;
        let start = first.saturating_sub(THUMB_PREFETCH);
        let end = (start + THUMB_VISIBLE + THUMB_PREFETCH * 2).min(self.pages.total);
        (start..end).map(PageNo::from_index).collect()
    }

    fn sync_page_input(&mut self) {
        if self.pages.total == 0 {
            self.page_input.clear();
            return;
        }
        self.page_input = (self.visible.index() + 1).to_string();
    }

    fn bump_render_gen(&mut self) {
        self.render_gen = self.render_gen.wrapping_add(1);
        if self.render_gen == 0 {
            self.render_gen = 1;
        }
    }

    fn navigate_to(&mut self, page: PageNo) {
        if self.go_to(page) {
            self.history.visit(self.visible);
            self.save_position();
        }
    }

    /// Núcleo sem registro: clamp + troca + geração. `true` se mudou de página.
    fn go_to(&mut self, page: PageNo) -> bool {
        if self.pages.total == 0 {
            self.sync_page_input();
            return false;
        }
        let max = self.pages.total - 1;
        let idx = page.index().min(max);
        let target = PageNo::from_index(idx);
        if target == self.visible {
            self.sync_page_input();
            return false;
        }
        self.visible = target;
        self.bump_render_gen();
        self.sync_page_input();
        true
    }

    /// Um passo no histórico (`forward` = avançar). Limites são no-op.
    fn history_go(&mut self, forward: bool) {
        // Página alcançada por rolagem (fora da pilha) entra antes de andar, para
        // o "voltar" cair de fato na página anterior; repetida é ignorada.
        self.history.visit(self.visible);
        let Some(page) = self.history.step(forward) else {
            return;
        };
        self.go_to(page);
        self.save_position();
    }

    pub fn can_history_back(&self) -> bool {
        self.history.can_back()
    }

    pub fn can_history_forward(&self) -> bool {
        self.history.can_forward()
    }

    /// Persiste página, zoom e modo atuais; silencioso se ilegível/erro.
    fn save_position(&self) {
        let path = self.source.path();
        let Some((size, mtime)) = file_identity(path) else {
            return;
        };
        let pos = DocPosition {
            page: self.visible.index(),
            zoom: self.zoom,
            mode: self.view_mode,
            size,
            mtime,
        };
        let entries = record_position(read_positions(), path.to_path_buf(), pos);
        let _ = save_positions(&entries);
    }

    /// Restaura página+zoom+modo do arquivo (identidade precisa); zera o histórico.
    fn restore_position(&mut self) {
        let path = self.source.path().to_path_buf();
        let Some(pos) = find_position(&read_positions(), &path) else {
            return;
        };
        self.zoom = pos.zoom;
        self.view_mode = pos.mode;
        let idx = pos.page.min(self.pages.total.saturating_sub(1));
        self.visible = PageNo::from_index(idx);
        if self.view_mode == ViewMode::Continuous {
            // Primeira renderização já parte da página restaurada.
            self.doc_scroll_y = self.page_offset(self.visible);
        }
        self.history.reset(self.visible);
    }

    fn apply_nav(&mut self, cmd: NavCmd) {
        match cmd {
            NavCmd::Previous => {
                let idx = self.visible.index().saturating_sub(1);
                self.navigate_to(PageNo::from_index(idx));
            }
            NavCmd::Next => {
                if self.pages.total == 0 {
                    self.sync_page_input();
                    return;
                }
                let max = self.pages.total - 1;
                let idx = (self.visible.index() + 1).min(max);
                self.navigate_to(PageNo::from_index(idx));
            }
            NavCmd::First => self.navigate_to(PageNo::first()),
            NavCmd::Last => {
                if self.pages.total == 0 {
                    self.sync_page_input();
                    return;
                }
                self.navigate_to(PageNo::from_index(self.pages.total - 1));
            }
            NavCmd::GoTo(page) => self.navigate_to(page),
        }
    }

    fn submit_page_input(&mut self) {
        let draft = self.page_input.trim();
        if draft.is_empty() {
            self.sync_page_input();
            return;
        }
        match draft.parse::<u32>() {
            Ok(0) => {
                if self.pages.total == 0 {
                    self.sync_page_input();
                    return;
                }
                self.navigate_to(PageNo::first());
            }
            Ok(one_based) if one_based >= 1 => {
                if self.pages.total == 0 {
                    self.sync_page_input();
                    return;
                }
                let idx = (one_based - 1).min(self.pages.total - 1);
                self.navigate_to(PageNo::from_index(idx));
            }
            _ => self.sync_page_input(),
        }
    }

    fn next_page_data_target(&self) -> Option<PageNo> {
        self.page_data_priority().into_iter().find(|page| {
            !self.has_page_data(*page)
                && !self.page_data_inflight.contains(&page.index())
                && !self.page_data_failed.contains(&page.index())
        })
    }

    fn page_data_priority(&self) -> Vec<PageNo> {
        let mut pages = vec![self.visible];
        let v = self.visible.index();
        if v + 1 < self.pages.total {
            pages.push(PageNo::from_index(v + 1));
        }
        if self.pages_open {
            for page in self.thumb_page_window() {
                if !pages.iter().any(|p| p.index() == page.index()) {
                    pages.push(page);
                }
            }
        }
        pages
    }

    fn prefetch_target(&self) -> Option<(PageNo, Scale, u8)> {
        if self.pages.total == 0 {
            return None;
        }
        if self.view_mode == ViewMode::Continuous {
            // Primeira da janela sem bitmap (visível primeiro, resto em ordem).
            let (start, end) = self.doc_window();
            let v = self.visible.index().min(end);
            for i in (v..end).chain(start..v) {
                let page = PageNo::from_index(i);
                if !self.has_page_data(page) {
                    continue;
                }
                let scale = self.page_scale(page);
                let key = render_key(page, scale, self.view_rotation);
                if self.surfaces.get(page, scale, self.view_rotation).is_none()
                    && !self.failed.contains(&key)
                {
                    return Some((page, scale, self.view_rotation));
                }
            }
            return None;
        }
        let next_idx = self.visible.index() + 1;
        if next_idx >= self.pages.total {
            return None;
        }
        let page = PageNo::from_index(next_idx);
        if !self.has_page_data(page) {
            return None;
        }
        Some((page, self.page_scale(page), self.view_rotation))
    }

    fn evict_unused(&mut self) {
        let keep = self.keep_pages();
        self.surfaces.retain_pages(&keep);
        self.surfaces
            .enforce_neighbor_budget(self.visible.index(), NEIGHBOR_CACHE_BUDGET);
        // Sem painel e sem diálogo: limpa tudo (comportamento antigo).
        // Com diálogo: retém o alvo do preview mesmo de painel fechado.
        let mut thumb_keep: HashSet<u32> = if self.pages_open {
            self.thumb_page_window()
                .into_iter()
                .map(|page| page.index())
                .collect()
        } else {
            HashSet::new()
        };
        if let Some(page) = self.print_preview_target() {
            thumb_keep.insert(page.index());
        }
        self.thumbs.retain_pages(&thumb_keep);
    }

    fn request_visible_render(&mut self) -> Option<Task<Message>> {
        if !self.render_inflight.is_empty() {
            return None;
        }
        if !self.has_page_data(self.visible) {
            return None;
        }
        let page = self.visible;
        let scale = self.page_scale(page);
        let rotation = self.view_rotation;
        let key = render_key(page, scale, rotation);
        if self.surfaces.get(page, scale, rotation).is_some() || self.failed.contains(&key) {
            return None;
        }
        self.track_render(key);
        Some(render_task(
            self.engine.clone(),
            page,
            scale,
            rotation,
            self.open_gen,
            self.render_gen,
        ))
    }

    fn request_speculative_render(&mut self) -> Option<Task<Message>> {
        if !self.render_inflight.is_empty() {
            return None;
        }
        if self
            .surface(self.visible, self.page_scale(self.visible))
            .is_none()
            || !self.has_page_data(self.visible)
        {
            return None;
        }
        let Some((page, scale, rotation)) = self.prefetch_target() else {
            return None;
        };
        let key = render_key(page, scale, rotation);
        if self.surfaces.get(page, scale, rotation).is_some() || self.failed.contains(&key) {
            return None;
        }
        if !self.prefetch_fits_budget(page, scale) {
            return None;
        }
        self.track_render(key);
        Some(render_task(
            self.engine.clone(),
            page,
            scale,
            rotation,
            self.open_gen,
            self.render_gen,
        ))
    }

    fn request_thumb_render(&mut self) -> Task<Message> {
        if !self.render_inflight.is_empty() || !self.page_data_inflight.is_empty() {
            return Task::none();
        }
        if !self.pages_open && self.print_dialog.is_none() {
            return Task::none();
        }
        let reading_ready = self.has_page_data(self.visible) || self.visible_surface().is_some();
        if !reading_ready {
            return Task::none();
        }
        // Alvo do preview primeiro; janela do painel em seguida (se aberto).
        let mut targets = if self.pages_open {
            self.thumb_page_window()
        } else {
            Vec::new()
        };
        if let Some(target) = self.print_preview_target() {
            if !targets.contains(&target) {
                targets.insert(0, target);
            }
        }
        for page in targets {
            let Some(media) = self.loaded_media(page) else {
                continue;
            };
            let scale = self.thumb_scale_for(media);
            let key = render_key(page, scale, 0);
            if self.thumbs.get(page, scale).is_some() || self.failed.contains(&key) {
                continue;
            }
            self.track_render(key);
            return render_task(
                self.engine.clone(),
                page,
                scale,
                0,
                self.open_gen,
                self.render_gen,
            );
        }
        Task::none()
    }

    fn track_render(&mut self, key: (u32, u16, u8)) {
        self.render_inflight.insert(key);
        self.render_inflight_gen.insert(key, self.render_gen);
        self.render_inflight_doc.insert(key, self.open_gen);
    }

    fn prefetch_fits_budget(&self, page: PageNo, scale: Scale) -> bool {
        let visible = self.visible.index();
        if page.index() == visible {
            return true;
        }
        let estimate = estimated_rgba_bytes(self.media(page), scale);
        let retained = self.surfaces.page_bytes(page.index());
        let neighbors = self.surfaces.neighbor_bytes(visible);
        neighbors.saturating_sub(retained).saturating_add(estimate) <= NEIGHBOR_CACHE_BUDGET
    }
}

pub type Document = Ready;

impl Document {
    fn from_bytes(source: OpenSource, bytes: Arc<[u8]>) -> Result<Ready, OpenError> {
        let engine =
            PdfiumEngine::open(bytes.clone()).map_err(|e| OpenError::Engine(e.to_string()))?;
        let pages = PageCatalog::extract_first(&engine)?;
        let signatures = analyze_pdf(bytes.as_ref()).map_err(|e| OpenError::Sign(e.to_string()))?;
        let mut ready = Ready {
            source,
            engine,
            pages,
            signatures,
            zoom: Zoom::Width,
            visible: PageNo::first(),
            history: History::new(PageNo::first()),
            view_mode: ViewMode::default(),
            view_rotation: 0,
            page_input: String::new(),
            search: Search::derive("", &[]),
            selection: None,
            annotations: Vec::new(),
            next_annot_id: 0,
            press_anchor: None,
            annot_undo: Vec::new(),
            annot_redo: Vec::new(),
            note_draft: None,
            note_drag: None,
            sheet_at: [0.0, 0.0],
            sheet_scroll: 0.0,
            signatures_open: false,
            pages_open: false,
            outline_open: false,
            outline: None,
            outline_collapsed: HashSet::new(),
            outline_cursor: None,
            outline_load_issued: false,
            pages_scroll_y: 0.0,
            doc_scroll_y: 0.0,
            recents: Vec::new(),
            render_scale: 1.0,
            theme: Theme::Dark,
            overflow_open: false,
            print_dialog: None,
            print_status: None,
            save_status: None,
            save_warning: false,
            open_gen: 0,
            render_gen: 1,
            surfaces: SurfaceCache::default(),
            thumbs: ThumbCache::default(),
            render_inflight: HashSet::new(),
            render_inflight_gen: HashMap::new(),
            render_inflight_doc: HashMap::new(),
            failed: HashSet::new(),
            page_data_inflight: HashSet::new(),
            page_data_failed: HashSet::new(),
            viewport: Viewport {
                width: 960.0,
                height: 720.0,
            },
        };
        ready.sync_page_input();
        Ok(ready)
    }
}

async fn open_ready(source: OpenSource) -> Result<Ready, OpenError> {
    let bytes = tokio::fs::read(source.path())
        .await
        .map_err(|e| OpenError::Io(e.to_string()))?;
    let bytes = Arc::<[u8]>::from(bytes);
    // Parse Pdfium + assinaturas fora do executor async: nada aqui pode
    // bloquear a janela.
    tokio::task::spawn_blocking(move || Document::from_bytes(source, bytes))
        .await
        .map_err(|e| OpenError::Engine(e.to_string()))?
}

impl PageCatalog {
    // ponytail: abre com a primeira pagina; resto por demanda em PageData
    fn extract_first(engine: &PdfiumEngine) -> Result<Self, OpenError> {
        let total = engine.page_count();
        let mut media: Vec<Option<MediaBox>> = vec![None; total as usize];
        let mut text: Vec<Option<TextLayer>> = vec![None; total as usize];
        if total > 0 {
            let (first_media, first_text) = engine
                .page_data(PageNo::first())
                .map_err(|e: EngineError| OpenError::Engine(e.to_string()))?;
            media[0] = Some(first_media);
            text[0] = Some(first_text);
        }
        Ok(PageCatalog { total, media, text })
    }
}

fn glyph_byte_range(layer: &TextLayer, index: usize) -> (usize, usize) {
    let mut cursor = 0usize;
    for (i, glyph) in layer.glyphs.iter().enumerate() {
        let next = cursor + glyph.cluster.len();
        if i == index {
            return (cursor, next);
        }
        cursor = next;
    }
    (0, 0)
}

/// Estende a seleção a partir da âncora do press: puxar de volta encolhe
/// (padrão dos leitores); sem âncora, ancora no cursor.
fn extend_range(anchor: Option<TextRange>, cursor: (usize, usize)) -> TextRange {
    let (start, end) = match anchor {
        Some(a) => (a.start, a.end),
        None => cursor,
    };
    TextRange {
        start: start.min(cursor.0),
        end: end.max(cursor.1),
    }
}

impl std::fmt::Debug for Ready {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ready")
            .field("source", &self.source)
            .field("zoom", &self.zoom)
            .field("visible", &self.visible)
            .field("view_mode", &self.view_mode)
            .field("signatures_open", &self.signatures_open)
            .field("pages_open", &self.pages_open)
            .finish()
    }
}

impl EmptyState {
    pub fn path_label(&self) -> String {
        display_path(self.cwd.as_deref())
    }

    pub fn parent(&self) -> Option<Option<PathBuf>> {
        match &self.cwd {
            None => None,
            Some(cwd) => Some(parent_of(cwd)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::Glyph;

    fn apply(session: &mut Session, message: Message) {
        let _ = session.update(message);
    }

    fn isolated<R>(f: impl FnOnce() -> R) -> R {
        let path = std::env::temp_dir().join(format!(
            "tsuro-session-recents-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let result = crate::browse::with_recents_path(path.clone(), f);
        let _ = std::fs::remove_file(path);
        result
    }

    /// Documento na tela (a aba ativa).
    fn active_ready(session: &Session) -> &Ready {
        match session {
            Session::Ready(tabs) => tabs.active(),
            other => panic!("esperava Ready, veio {other:?}"),
        }
    }

    /// Zoom manual em fator (os assertos de aba comparam números).
    fn zoom_factor(ready: &Ready) -> f32 {
        match ready.zoom {
            Zoom::Manual(factor) => factor.get(),
            other => panic!("esperava zoom manual, veio {other:?}"),
        }
    }

    /// Issue #40: uma aba por documento, cada uma com a sua página e o seu
    /// zoom; a última aba fechada fecha a janela, como antes.
    #[test]
    fn tabs_keep_page_and_zoom_per_document() {
        isolated(|| {
            let Some(first) = sample_ready() else {
                return;
            };
            let Some(second) = sample_ready() else {
                return;
            };
            if first.page_count() < 2 {
                return;
            }
            let mut session = Session::Ready(Tabs::single(first));
            // Aba 1: página 2, zoom 2×.
            apply(
                &mut session,
                Message::Nav(NavCmd::GoTo(PageNo::from_index(1))),
            );
            apply(
                &mut session,
                Message::SetZoom(Zoom::Manual(ZoomFactor::new(2.0))),
            );

            // Abrir com uma aba aberta entra em aba nova (não substitui).
            let _ = session.begin_open(second.source.clone());
            session.finish_open(Ok(second));
            apply(&mut session, Message::Nav(NavCmd::GoTo(PageNo::first())));
            apply(
                &mut session,
                Message::SetZoom(Zoom::Manual(ZoomFactor::new(1.0))),
            );
            {
                let Session::Ready(tabs) = &session else {
                    panic!("esperava Ready, veio {session:?}");
                };
                assert_eq!(tabs.len(), 2, "o segundo documento entra como aba");
                assert_eq!(tabs.active_index(), 1, "a aba nova entra ativa");
                assert_eq!(zoom_factor(&tabs.docs()[0]), 2.0);
                assert_eq!(tabs.docs()[0].visible.index(), 1);
                assert_eq!(zoom_factor(&tabs.docs()[1]), 1.0);
                assert_eq!(tabs.docs()[1].visible.index(), 0);
            }

            // Trocar de aba devolve cada documento no seu estado.
            apply(&mut session, Message::SelectTab(0));
            assert_eq!(active_ready(&session).visible.index(), 1);
            assert_eq!(zoom_factor(active_ready(&session)), 2.0);
            apply(&mut session, Message::CycleTab(1));
            assert_eq!(active_ready(&session).visible.index(), 0);
            assert_eq!(zoom_factor(active_ready(&session)), 1.0);

            // Fechar uma aba volta para a outra, intacta.
            apply(&mut session, Message::CloseTab(1));
            {
                let Session::Ready(tabs) = &session else {
                    panic!("esperava Ready, veio {session:?}");
                };
                assert_eq!(tabs.len(), 1);
                assert_eq!(tabs.active_index(), 0);
            }
            assert_eq!(active_ready(&session).visible.index(), 1);
            assert_eq!(zoom_factor(active_ready(&session)), 2.0);

            // A última aba fecha a janela (comportamento de sempre).
            apply(&mut session, Message::CloseTabActive);
            assert!(matches!(session, Session::Empty(_)));
        });
    }

    /// Abrir com o motor ausente não derruba a janela: a aba pendente falha e
    /// as que já estavam abertas seguem na tela.
    #[test]
    fn failed_second_open_keeps_open_tabs() {
        isolated(|| {
            let Some(ready) = sample_ready() else {
                return;
            };
            let source = ready.source.clone();
            let mut session = Session::Ready(Tabs::single(ready));
            let _ = session.begin_open(source);
            session.finish_open(Err(OpenError::Engine("motor quebrou".into())));
            let Session::Ready(tabs) = &session else {
                panic!("esperava Ready, veio {session:?}");
            };
            assert_eq!(tabs.len(), 1);
            assert!(tabs
                .open_error()
                .is_some_and(|msg| msg.contains("motor quebrou")));
        });
    }

    #[test]
    fn search_keeps_portuguese_accents() {
        let layer = TextLayer {
            page: PageNo::first(),
            plain: "texto ação extra".into(),
            glyphs: vec![Glyph {
                cluster: "texto ação extra".into(),
                quad: Quad::from_rect(0.0, 0.0, 10.0, 10.0),
            }],
        };
        let hits = Search::derive("ação", &[Some(layer.clone())]);
        assert_eq!(hits.hits().len(), 1);
        let none = Search::derive("acao", &[Some(layer)]);
        assert!(none.hits().is_empty());
    }

    #[test]
    fn lazy_search_skips_unloaded_pages() {
        let layer = TextLayer {
            page: PageNo::first(),
            plain: "texto ação extra".into(),
            glyphs: vec![Glyph {
                cluster: "texto ação extra".into(),
                quad: Quad::from_rect(0.0, 0.0, 10.0, 10.0),
            }],
        };
        let mut pages: Vec<Option<TextLayer>> = vec![None; 200];
        pages[0] = Some(layer);
        let hits = Search::derive("ação", &pages);
        assert_eq!(hits.hits().len(), 1);
        assert_eq!(pages.len(), 200);
    }

    #[test]
    fn zoom_width_uses_media_box() {
        let media = MediaBox {
            width: 400.0,
            height: 800.0,
        };
        let viewport = Viewport {
            width: 800.0,
            height: 600.0,
        };
        let scale = Zoom::Width.scale(viewport, media);
        assert!((scale.factor() - 2.0).abs() < 0.002);
    }

    #[test]
    fn zoom_change_keeps_old_bitmap_until_rerender() {
        // Tela não apaga ao trocar o zoom: visible_surface devolve o bitmap
        // da escala antiga até o render da nova chegar.
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let page = ready.visible;
        let old_scale = ready.page_scale(page);
        ready
            .surfaces
            .insert(page, old_scale, 0, fake_surface(page, old_scale));
        assert!(ready.visible_surface().is_some());
        // Nova escala sem render: o cache não tem a chave exata…
        ready.render_scale = 2.0;
        let new_scale = ready.page_scale(page);
        assert_ne!(new_scale, old_scale);
        assert!(ready.surface(page, new_scale).is_none());
        // …mas a tela segue mostrando o bitmap antigo.
        assert!(ready.visible_surface().is_some());
    }

    fn fake_surface(page: PageNo, scale: Scale) -> PageSurface {
        let _ = page;
        PageSurface {
            bitmap: crate::page::Bitmap {
                width: 2,
                height: 2,
                rgba: vec![0; 16],
            },
            scale,
        }
    }

    #[test]
    fn empty_session_is_still_no_document() {
        assert!(matches!(Session::empty(), Session::Empty(_)));
        assert!(matches!(
            Session::open_path(PathBuf::from("/tmp/doc.pdf")),
            Session::Loading { .. }
        ));
    }

    #[test]
    fn listing_error_stays_empty() {
        let mut session = Session::empty();
        apply(
            &mut session,
            Message::ListingReady {
                path: None,
                result: Err("sem permissão".into()),
            },
        );
        match &session {
            Session::Empty(empty) => {
                assert_eq!(empty.listing_error.as_deref(), Some("sem permissão"));
                assert!(empty.listing.is_empty());
            }
            other => panic!("listing error left Empty, got {other:?}"),
        }
    }

    #[test]
    fn browse_into_folder_and_back() {
        let folder = PathBuf::from("/tmp/tsuro-docs");
        let mut session = Session::empty();
        apply(
            &mut session,
            Message::ListingReady {
                path: None,
                result: Ok(vec![FsEntry {
                    path: folder.clone(),
                    name: "docs".into(),
                    is_dir: true,
                }]),
            },
        );
        apply(&mut session, Message::BrowseTo(Some(folder.clone())));
        match &session {
            Session::Empty(empty) => assert_eq!(empty.cwd.as_deref(), Some(folder.as_path())),
            other => panic!("expected Empty after BrowseTo, got {other:?}"),
        }
        apply(
            &mut session,
            Message::ListingReady {
                path: Some(folder.clone()),
                result: Ok(vec![FsEntry {
                    path: folder.join("a.pdf"),
                    name: "a.pdf".into(),
                    is_dir: false,
                }]),
            },
        );
        match &session {
            Session::Empty(empty) => {
                assert_eq!(empty.listing.len(), 1);
                assert!(!empty.listing[0].is_dir);
            }
            other => panic!("expected listing, got {other:?}"),
        }
        apply(&mut session, Message::BrowseTo(None));
        match &session {
            Session::Empty(empty) => assert!(empty.cwd.is_none()),
            other => panic!("expected roots, got {other:?}"),
        }
    }

    #[test]
    fn open_path_skips_browser() {
        let session = Session::open_path(PathBuf::from("/tmp/direct.pdf"));
        assert!(matches!(session, Session::Loading { .. }));
        assert!(!matches!(session, Session::Empty(_)));
    }

    #[test]
    fn remember_recent_on_successful_open_and_close() {
        isolated(|| {
            let pdf = PathBuf::from("/tmp/remembered.pdf");
            let mut session = Session::Empty(EmptyState {
                recents: vec![PathBuf::from("/tmp/older.pdf")],
                ..EmptyState::default()
            });
            let _ = session.begin_open(OpenSource::Path(pdf.clone()));
            session.finish_open(Err(OpenError::Engine("sem motor no teste".into())));
            match &session {
                Session::Failed { recents, .. } => {
                    assert!(recents.contains(&PathBuf::from("/tmp/older.pdf")));
                }
                other => panic!("expected Failed, got {other:?}"),
            }
            let Some(ready) = sample_ready() else {
                return;
            };
            let mut session = Session::Empty(EmptyState {
                recents: vec![PathBuf::from("/tmp/older.pdf")],
                ..EmptyState::default()
            });
            let _ = session.begin_open(ready.source.clone());
            session.finish_open(Ok(ready));
            match &session {
                Session::Ready(ready) => {
                    assert!(!ready.signatures_open);
                    assert!(!ready.pages_open);
                    assert_eq!(
                        ready.recents.first(),
                        Some(&ready.source.path().to_path_buf())
                    );
                    assert!(ready.recents.contains(&PathBuf::from("/tmp/older.pdf")));
                }
                other => panic!("expected Ready, got {other:?}"),
            }
            apply(&mut session, Message::Close);
            match &session {
                Session::Empty(empty) => {
                    assert_eq!(
                        empty.recents.first().and_then(|p| p.file_name()),
                        sample_pdf().file_name()
                    );
                }
                other => panic!("Close should return to Empty, got {other:?}"),
            }
        });
    }

    #[test]
    fn close_from_failed_returns_to_empty() {
        isolated(|| {
            let mut session = Session::Empty(EmptyState::default());
            let _ = session.begin_open(OpenSource::Path(PathBuf::from("/tmp/falha.pdf")));
            session.finish_open(Err(OpenError::Engine("motor quebrou".into())));
            assert!(matches!(session, Session::Failed { .. }));
            apply(&mut session, Message::Close);
            assert!(matches!(session, Session::Empty(_)));
        });
    }

    #[test]
    fn dead_recent_drops_after_io_failure() {
        isolated(|| {
            let missing = PathBuf::from("/tmp/tsuro-missing-recent.pdf");
            let mut session = Session::Empty(EmptyState {
                recents: vec![missing.clone(), PathBuf::from("/tmp/keep.pdf")],
                ..EmptyState::default()
            });
            let _ = session.begin_open(OpenSource::Path(missing.clone()));
            session.finish_open(Err(OpenError::Io("arquivo em falta".into())));
            match &session {
                Session::Failed {
                    recents, message, ..
                } => {
                    assert!(message.contains("arquivo em falta"));
                    assert!(!recents.contains(&missing));
                    assert!(recents.contains(&PathBuf::from("/tmp/keep.pdf")));
                }
                other => panic!("expected Failed, got {other:?}"),
            }
        });
    }

    #[test]
    fn ready_panels_start_closed_and_toggle_independently() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        match &session {
            Session::Ready(ready) => {
                assert!(!ready.signatures_open);
                assert!(!ready.pages_open);
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::ToggleSignatures);
        match &session {
            Session::Ready(ready) => {
                assert!(ready.signatures_open);
                assert!(!ready.pages_open);
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::TogglePages);
        match &session {
            Session::Ready(ready) => {
                assert!(ready.signatures_open);
                assert!(ready.pages_open);
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::ToggleSignatures);
        match &session {
            Session::Ready(ready) => {
                assert!(!ready.signatures_open);
                assert!(ready.pages_open);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn set_page_from_list_changes_visible() {
        let Some(ready) = sample_ready() else {
            return;
        };
        if ready.page_count() < 2 {
            return;
        }
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::Nav(NavCmd::GoTo(PageNo::from_index(1))),
        );
        match &session {
            Session::Ready(ready) => assert_eq!(ready.visible.index(), 1),
            _ => unreachable!(),
        }
    }

    /// Geometria determinística: mídias uniformes 100×200, sem painel.
    fn uniform_ready() -> Option<Ready> {
        let mut ready = sample_ready()?;
        for media in ready.pages.media.iter_mut() {
            *media = Some(MediaBox {
                width: 100.0,
                height: 200.0,
            });
        }
        ready.viewport = Viewport {
            width: 800.0,
            height: 600.0,
        };
        ready.pages_open = false;
        ready.signatures_open = false;
        Some(ready)
    }

    /// Zoom out encolhe a folha, não o palco: manual 25% numa mídia 100pt →
    /// folha 25px, palco na moldura útil (sem barra horizontal). Zoom in 8× →
    /// folha 800px, palco acompanha (com barra horizontal).
    #[test]
    fn sheet_scales_with_zoom_and_stage_covers_it() {
        let Some(mut ready) = uniform_ready() else {
            return;
        };
        let page = PageNo::first();
        let pane = 744.0 + 2.0 * DOC_PAD_X;
        // Ajuste à largura: folha coincide com a largura útil.
        ready.zoom = Zoom::Width;
        assert!((ready.sheet_width(page) - 744.0).abs() < 0.001);
        // Zoom out: folha 25px, palco segue na moldura (menor que o painel).
        ready.zoom = Zoom::Manual(ZoomFactor::new(0.25));
        assert!((ready.sheet_width(page) - 25.0).abs() < 0.001);
        assert!((ready.doc_stage_width(page) - (pane - 1.0)).abs() < 0.001);
        assert!(ready.doc_stage_width(page) < pane);
        // Zoom in: folha 800px, palco acompanha para rolar.
        ready.zoom = Zoom::Manual(ZoomFactor::new(8.0));
        assert!((ready.sheet_width(page) - 800.0).abs() < 0.001);
        assert!((ready.doc_stage_width(page) - (800.0 + 2.0 * DOC_PAD_X)).abs() < 0.001);
        assert!(ready.doc_stage_width(page) > pane);
        // Contínuo cobre a página mais larga.
        assert!((ready.doc_stage_max_width() - ready.doc_stage_width(page)).abs() < 0.001);
    }

    /// Largura útil 744 (800 − 8 − 48); célula e passo derivam dos pads
    /// (não literais: DOC_PAD_BOTTOM já mudou 32→56 uma vez).
    #[test]
    fn continuous_offsets_match_cell_geometry() {
        let Some(ready) = uniform_ready() else {
            return;
        };
        assert_eq!(ready.doc_content_width(), 744.0);
        let cell = DOC_PAD_TOP + 744.0 * 2.0 + DOC_PAD_BOTTOM;
        let n = ready.page_count();
        assert_eq!(ready.page_offset(PageNo::first()), 0.0);
        if n > 1 {
            assert_eq!(ready.page_offset(PageNo::from_index(1)), cell + DOC_GAP);
        }
        let last = PageNo::from_index(n - 1);
        assert_eq!(ready.doc_total_height(), ready.page_offset(last) + cell);
    }

    #[test]
    fn page_at_offset_resolves_borders_and_clamps() {
        let Some(ready) = uniform_ready() else {
            return;
        };
        let cell = DOC_PAD_TOP + 744.0 * 2.0 + DOC_PAD_BOTTOM;
        let last = ready.page_count() - 1;
        assert_eq!(ready.page_at_offset(0.0).index(), 0);
        assert_eq!(ready.page_at_offset(cell - 1.0).index(), 0);
        // No gap entre células, a próxima página já responde.
        let gap_page = if last > 0 { 1 } else { 0 };
        assert_eq!(ready.page_at_offset(cell).index(), gap_page);
        assert_eq!(ready.page_at_offset(-5.0).index(), 0);
        assert_eq!(ready.page_at_offset(1e9).index(), last);
    }

    #[test]
    fn view_mode_switch_preserves_visible_and_zoom() {
        let Some(ready) = uniform_ready() else {
            return;
        };
        assert_eq!(ready.view_mode, ViewMode::Single);
        let target = 1.min(ready.page_count() - 1);
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::Nav(NavCmd::GoTo(PageNo::from_index(target))),
        );
        apply(&mut session, Message::SetViewMode(ViewMode::Continuous));
        match &session {
            Session::Ready(ready) => {
                assert_eq!(ready.view_mode, ViewMode::Continuous);
                assert_eq!(ready.visible.index(), target);
                assert!(matches!(ready.zoom, Zoom::Width));
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::SetViewMode(ViewMode::Single));
        match &session {
            Session::Ready(ready) => assert_eq!(ready.view_mode, ViewMode::Single),
            _ => unreachable!(),
        }
    }

    fn outline_tree() -> Outline {
        Outline {
            items: vec![
                OutlineItem {
                    title: "A".to_string(),
                    page: PageNo::first(),
                    children: vec![
                        OutlineItem {
                            title: "A1".to_string(),
                            page: PageNo::from_index(2),
                            children: vec![],
                        },
                        OutlineItem {
                            title: "A2".to_string(),
                            page: PageNo::from_index(5),
                            children: vec![],
                        },
                    ],
                },
                OutlineItem {
                    title: "B".to_string(),
                    page: PageNo::from_index(8),
                    children: vec![],
                },
            ],
        }
    }

    #[test]
    fn doc_scrolled_derives_visible_in_continuous() {
        let Some(ready) = uniform_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        apply(&mut session, Message::SetViewMode(ViewMode::Continuous));
        // Rolar não muda nada em página única; aqui deve derivar a página 2.
        apply(&mut session, Message::DocScrolled(2.0 * 1564.0 + 10.0));
        match &session {
            Session::Ready(ready) => {
                assert_eq!(ready.visible.index(), 2.min(ready.page_count() - 1));
                assert_eq!(ready.page_input(), (ready.visible.index() + 1).to_string());
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn doc_window_contains_visible_and_clamps() {
        let Some(ready) = uniform_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        apply(&mut session, Message::SetViewMode(ViewMode::Continuous));
        apply(&mut session, Message::DocScrolled(0.0));
        match &session {
            Session::Ready(ready) => {
                let (start, end) = ready.doc_window();
                let total = ready.page_count();
                assert!(start <= ready.visible.index());
                assert!(ready.visible.index() < end.max(1));
                assert!(end <= total);
                // Em contínuo o cache retém a janela; em única, visible ± 1.
                let keep = ready.keep_pages();
                for i in start..end {
                    assert!(keep.contains(&i));
                }
                assert!(!keep.is_empty());
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn history_back_forward_and_truncation() {
        let Some(ready) = sample_ready() else {
            return;
        };
        if ready.page_count() < 2 {
            return;
        }
        let last = ready.page_count().saturating_sub(1);
        let first = PageNo::first();
        let last_page = PageNo::from_index(last);
        let mut session = Session::Ready(Tabs::single(ready));
        let visible = |s: &Session| match s {
            Session::Ready(r) => r.visible,
            _ => unreachable!(),
        };
        let hist_len = |s: &Session| match s {
            Session::Ready(r) => (r.history.pages.len(), r.history.pos),
            _ => unreachable!(),
        };
        assert_eq!(hist_len(&session), (1, 0));
        apply(&mut session, Message::Nav(NavCmd::GoTo(last_page)));
        assert_eq!(visible(&session), last_page);
        assert_eq!(hist_len(&session), (2, 1));
        apply(&mut session, Message::Nav(NavCmd::GoTo(first)));
        assert_eq!(hist_len(&session), (3, 2));
        // Mesma página não duplica.
        apply(&mut session, Message::Nav(NavCmd::GoTo(first)));
        assert_eq!(hist_len(&session), (3, 2));
        apply(&mut session, Message::HistoryBack);
        assert_eq!(visible(&session), last_page);
        // Navegar descarta o "futuro".
        apply(&mut session, Message::Nav(NavCmd::GoTo(first)));
        assert_eq!(hist_len(&session), (3, 2));
        apply(&mut session, Message::HistoryBack);
        apply(&mut session, Message::HistoryBack);
        assert_eq!(visible(&session), first);
        assert_eq!(hist_len(&session), (3, 0));
        // Limite é no-op.
        apply(&mut session, Message::HistoryBack);
        assert_eq!(visible(&session), first);
        apply(&mut session, Message::HistoryForward);
        assert_eq!(visible(&session), last_page);
        apply(&mut session, Message::HistoryForward);
        assert_eq!(visible(&session), first);
        apply(&mut session, Message::HistoryForward);
        assert_eq!(visible(&session), first);
        match &session {
            Session::Ready(r) => {
                assert!(!r.can_history_back() || r.history.pos > 0);
                assert_eq!(r.can_history_forward(), r.history.can_forward());
            }
            _ => unreachable!(),
        }
    }

    /// Pilha pura (sem documento/fixture): 1→5→3, voltar/avançar, colapso,
    /// reset e página alcançada por rolagem entrando antes de andar.
    #[test]
    fn history_stack_walks_visits_without_document() {
        let (p1, p5, p3) = (
            PageNo::first(),
            PageNo::from_index(4),
            PageNo::from_index(2),
        );
        let mut history = History::new(p1);
        assert_eq!(history.current(), p1);
        history.visit(p5);
        history.visit(p3);
        assert_eq!(history.pages, vec![p1, p5, p3]);
        assert_eq!(history.pos, 2);
        // Voltar/avançar como navegador.
        assert_eq!(history.step(false), Some(p5));
        assert_eq!(history.current(), p5);
        assert_eq!(history.step(false), Some(p1));
        assert_eq!(history.current(), p1);
        // Limite é no-op.
        assert_eq!(history.step(false), None);
        assert_eq!(history.current(), p1);
        assert_eq!(history.step(true), Some(p5));
        // Visita nova descarta o "futuro".
        history.visit(p3);
        assert_eq!(history.pages, vec![p1, p5, p3]);
        assert!(!history.can_forward());
        // Repetida consecutiva não entra.
        history.visit(p3);
        assert_eq!(history.pages.len(), 3);
        // Rolagem leva a página fora da pilha: entra antes de andar.
        let p9 = PageNo::from_index(8);
        history.visit(p9);
        assert_eq!(history.step(false), Some(p3));
        assert_eq!(history.step(true), Some(p9));
        // Abrir outro documento (ou restaurar posição) zera a pilha.
        history.reset(p5);
        assert_eq!(history.pages, vec![p5]);
        assert!(!history.can_back());
        assert!(!history.can_forward());
        assert_eq!(history.step(false), None);
    }

    /// Busca salta para o hit e o "voltar" devolve a página original (fixture
    /// real para a contagem de páginas; camada de texto injetada como o engine
    /// entregaria).
    #[test]
    fn search_jump_then_back_returns_to_reading_page() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let last = ready.page_count().saturating_sub(1);
        if last < 1 {
            return;
        }
        let hit_page = PageNo::from_index(last);
        ready.pages.text[last as usize] = Some(TextLayer {
            page: hit_page,
            plain: "cláusula".into(),
            glyphs: vec![Glyph {
                cluster: "cláusula".into(),
                quad: Quad::from_rect(0.0, 0.0, 10.0, 10.0),
            }],
        });
        let file = std::env::temp_dir().join(format!(
            "tsuro-positions-unit-{}-search",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file);
        crate::positions::with_positions_path(file.clone(), || {
            let mut session = Session::Ready(Tabs::single(ready));
            let visible = |s: &Session| match s {
                Session::Ready(r) => r.visible,
                _ => unreachable!(),
            };
            apply(&mut session, Message::SearchChanged("cláusula".into()));
            assert_eq!(visible(&session), hit_page);
            apply(&mut session, Message::HistoryBack);
            assert_eq!(visible(&session), PageNo::first());
            apply(&mut session, Message::HistoryForward);
            assert_eq!(visible(&session), hit_page);
        });
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn history_shortcut_uses_history_messages() {
        use iced::event::Status;
        use iced::keyboard::Modifiers;
        // Alt+←/→ (⌘ no mac); outros modificadores continuam ignorados.
        #[cfg(target_os = "macos")]
        let hist = Modifiers::LOGO;
        #[cfg(not(target_os = "macos"))]
        let hist = Modifiers::ALT;
        assert!(matches!(
            keyboard_message(Key::Named(Named::ArrowLeft), hist, Status::Ignored),
            Some(Message::HistoryBack)
        ));
        assert!(matches!(
            keyboard_message(Key::Named(Named::ArrowRight), hist, Status::Ignored),
            Some(Message::HistoryForward)
        ));
        assert!(keyboard_message(
            Key::Named(Named::ArrowLeft),
            Modifiers::SHIFT,
            Status::Ignored
        )
        .is_none());
    }

    #[test]
    fn annot_shortcuts_mark_undo_redo() {
        use iced::event::Status;
        use iced::keyboard::Modifiers;
        let plain = Modifiers::empty();
        assert!(matches!(
            keyboard_message(Key::Character("h".into()), plain, Status::Ignored),
            Some(Message::Annotate(AnnotKind::Highlight))
        ));
        assert!(matches!(
            keyboard_message(Key::Character("U".into()), plain, Status::Ignored),
            Some(Message::Annotate(AnnotKind::Underline))
        ));
        assert!(matches!(
            keyboard_message(Key::Character("s".into()), plain, Status::Ignored),
            Some(Message::Annotate(AnnotKind::Strikeout))
        ));
        assert!(matches!(
            keyboard_message(Key::Character("n".into()), plain, Status::Ignored),
            Some(Message::Annotate(AnnotKind::Note))
        ));
        assert!(matches!(
            keyboard_message(Key::Character("N".into()), plain, Status::Ignored),
            Some(Message::Annotate(AnnotKind::Note))
        ));
        // Com modificador o atalho não dispara (mesma guarda de H/U/S).
        assert!(keyboard_message(
            Key::Character("n".into()),
            Modifiers::SHIFT,
            Status::Ignored
        )
        .is_none());
        #[cfg(target_os = "macos")]
        let cmd = Modifiers::LOGO;
        #[cfg(not(target_os = "macos"))]
        let cmd = Modifiers::CTRL;
        assert!(matches!(
            keyboard_message(Key::Character("z".into()), cmd, Status::Ignored),
            Some(Message::AnnotUndo)
        ));
        assert!(matches!(
            keyboard_message(
                Key::Character("Z".into()),
                cmd | Modifiers::SHIFT,
                Status::Ignored
            ),
            Some(Message::AnnotRedo)
        ));
        // Com foco em campo o iced captura antes (guarda de foco).
        assert!(keyboard_message(Key::Character("h".into()), plain, Status::Captured).is_none());
    }

    #[test]
    fn annotate_clears_selection_and_unfocus_kills_drag() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        ready.selection = Some(Selection {
            page: PageNo::first(),
            range: TextRange { start: 0, end: 2 },
        });
        let mut session = Session::Ready(Tabs::single(ready));
        // Marcar limpa a seleção (padrão dos leitores).
        apply(&mut session, Message::Annotate(AnnotKind::Highlight));
        match &session {
            Session::Ready(ready) => {
                assert_eq!(ready.annotations.len(), 1);
                assert!(ready.selection.is_none());
            }
            _ => unreachable!(),
        }
        // Unfocus no meio do drag mata a âncora, mantém a seleção.
        match &mut session {
            Session::Ready(ready) => {
                ready.selection = Some(Selection {
                    page: PageNo::first(),
                    range: TextRange { start: 0, end: 2 },
                });
                ready.press_anchor = Some(PressAnchor {
                    sel: Selection {
                        page: PageNo::first(),
                        range: TextRange { start: 0, end: 2 },
                    },
                    exact: true,
                });
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::DragCancelled);
        match &session {
            Session::Ready(ready) => {
                assert!(ready.press_anchor.is_none());
                assert!(ready.selection.is_some());
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn display_rect_roundtrips_page_pt_in_all_rotations() {
        let media = MediaBox {
            width: 100.0,
            height: 200.0,
        };
        let quad = Quad::from_rect(10.0, 20.0, 30.0, 60.0);
        for rot in 0..4u8 {
            let (dw, dh) = if rot & 1 == 1 {
                (400.0, 200.0)
            } else {
                (200.0, 400.0)
            };
            let r = display_rect(quad, media, rot, dw, dh);
            // 20×40 pt vira 40×80 px; rotação ímpar troca os eixos (80×40).
            let (ew, eh) = if rot & 1 == 1 {
                (80.0, 40.0)
            } else {
                (40.0, 80.0)
            };
            assert!((r[2] - ew).abs() < 0.01, "rot {rot}: {r:?}");
            assert!((r[3] - eh).abs() < 0.01, "rot {rot}: {r:?}");
            // Centro exibido volta ao centro original.
            let pt = page_pt_at([r[0] + r[2] / 2.0, r[1] + r[3] / 2.0], media, rot, dw, dh);
            assert!((pt[0] - 20.0).abs() < 0.02, "rot {rot}: {pt:?}");
            assert!((pt[1] - 40.0).abs() < 0.02, "rot {rot}: {pt:?}");
        }
        // Âncora absoluta de orientação (espaço PDF tem Y para cima, tela
        // para baixo): faixa na base do PDF aparece na base da tela.
        let bottom = display_rect(
            Quad::from_rect(0.0, 0.0, 100.0, 20.0),
            media,
            0,
            200.0,
            400.0,
        );
        assert!((bottom[1] - 360.0).abs() < 0.01, "flip Y: {bottom:?}");
        assert!((bottom[3] - 40.0).abs() < 0.01, "flip Y: {bottom:?}");
        // Topo da tela volta ao topo do PDF.
        let pt = page_pt_at([100.0, 0.0], media, 0, 200.0, 400.0);
        assert!((pt[0] - 50.0).abs() < 0.01, "flip Y: {pt:?}");
        assert!((pt[1] - 200.0).abs() < 0.01, "flip Y: {pt:?}");
    }

    #[test]
    fn annotate_selection_undo_redo_remove() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        // Sem seleção: ignora.
        assert!(!ready.annotate_selection(AnnotKind::Highlight));
        assert!(ready.annotations.is_empty());
        let len = match ready.pages.text.first() {
            Some(Some(layer)) => layer.plain.len(),
            _ => return,
        };
        if len < 2 {
            return;
        }
        let sel = Selection {
            page: PageNo::first(),
            range: TextRange { start: 0, end: 2 },
        };
        ready.selection = Some(sel.clone());
        assert!(ready.annotate_selection(AnnotKind::Highlight));
        assert_eq!(ready.annotations.len(), 1);
        assert_eq!(ready.annotations[0].kind, AnnotKind::Highlight);
        assert!(ready.can_annot_undo());
        // Undo esvazia, redo restaura.
        assert!(ready.annot_undo_once());
        assert!(ready.annotations.is_empty());
        assert!(ready.can_annot_redo());
        assert!(ready.annot_redo_once());
        assert_eq!(ready.annotations.len(), 1);
        // Nova ação limpa o redo.
        assert!(ready.annot_undo_once());
        assert!(ready.annotate_selection(AnnotKind::Underline));
        assert!(!ready.can_annot_redo());
        // Clique sobre a marcação acha o id; fora não.
        let annot = ready.annotations[0].clone();
        assert!(!annot.quads.is_empty());
        let q = annot.quads[0];
        let xs = [q.x0, q.x1, q.x2, q.x3];
        let ys = [q.y0, q.y1, q.y2, q.y3];
        let center = [
            (xs.iter().fold(f32::INFINITY, |a, &b| a.min(b))
                + xs.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b)))
                / 2.0,
            (ys.iter().fold(f32::INFINITY, |a, &b| a.min(b))
                + ys.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b)))
                / 2.0,
        ];
        assert_eq!(ready.annotation_at(annot.page, center), Some(annot.id));
        assert_eq!(ready.annotation_at(annot.page, [-1000.0, -1000.0]), None);
        // PointerUp sem arrasto remove via mensagens.
        ready.press_anchor = Some(PressAnchor { sel, exact: true });
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::PointerUp {
                page: annot.page,
                page_pt: center,
            },
        );
        match &session {
            Session::Ready(ready) => assert!(ready.annotations.is_empty()),
            _ => unreachable!(),
        }
    }

    /// Draft de teste: âncora na página 1, trecho 0..2, sem depender do
    /// texto extraído do PDF (os testes de nota mexem só no estado).
    fn dummy_draft() -> NoteDraft {
        dummy_draft_with("")
    }

    /// Draft de teste com texto digitado (o conteúdo é o editor do iced).
    fn dummy_draft_with(text: &str) -> NoteDraft {
        NoteDraft {
            page: PageNo::first(),
            range: TextRange { start: 0, end: 2 },
            quads: vec![Quad::from_rect(0.0, 0.0, 10.0, 10.0)],
            content: text_editor::Content::with_text(text),
            editing: None,
            anchor: [0.0, 0.0],
            anchor_scroll: 0.0,
        }
    }

    #[test]
    fn note_draft_save_creates_note_with_text() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        assert!(!ready.can_annot_undo());
        ready.note_draft = Some(dummy_draft_with("olá mundo"));
        assert!(ready.save_note_draft());
        // Draft fechou e a nota entrou no estado com o texto digitado.
        assert!(ready.note_draft.is_none());
        assert_eq!(ready.annotations.len(), 1);
        assert_eq!(ready.annotations[0].kind, AnnotKind::Note);
        assert_eq!(ready.annotations[0].text, "olá mundo");
        assert!(ready.can_annot_undo());
    }

    #[test]
    fn note_draft_save_whitespace_discards_without_undo_entry() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        ready.note_draft = Some(dummy_draft_with("   \n"));
        assert!(!ready.save_note_draft());
        // Fecha o draft mas não cria nada nem suja a pilha de undo.
        assert!(ready.note_draft.is_none());
        assert!(ready.annotations.is_empty());
        assert!(!ready.can_annot_undo());
        assert!(!ready.can_annot_redo());
    }

    #[test]
    fn note_add_undo_redo() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        ready.note_draft = Some(dummy_draft_with("nota"));
        assert!(ready.save_note_draft());
        assert_eq!(ready.annotations.len(), 1);
        // Undo remove a nota; redo a devolve com o texto.
        assert!(ready.annot_undo_once());
        assert!(ready.annotations.is_empty());
        assert!(!ready.can_annot_undo());
        assert!(ready.can_annot_redo());
        assert!(ready.annot_redo_once());
        assert_eq!(ready.annotations.len(), 1);
        assert_eq!(ready.annotations[0].kind, AnnotKind::Note);
        assert_eq!(ready.annotations[0].text, "nota");
    }

    #[test]
    fn note_edit_undo_restores_old_text() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        // Nota existente.
        ready.note_draft = Some(dummy_draft_with("antiga"));
        assert!(ready.save_note_draft());
        let id = ready.annotations[0].id;
        // Edição (draft com editing = Some(id)): Remove(antiga) + Add(nova).
        let mut draft = dummy_draft_with("nova");
        draft.editing = Some(id);
        ready.note_draft = Some(draft);
        assert!(ready.save_note_draft());
        assert_eq!(ready.annotations.len(), 1);
        assert_ne!(ready.annotations[0].id, id);
        assert_eq!(ready.annotations[0].text, "nova");
        // Desfaz a edição: primeiro some, segundo restaura o texto antigo.
        assert!(ready.annot_undo_once());
        assert!(ready.annotations.is_empty());
        assert!(ready.annot_undo_once());
        assert_eq!(ready.annotations.len(), 1);
        assert_eq!(ready.annotations[0].id, id);
        assert_eq!(ready.annotations[0].text, "antiga");
    }

    #[test]
    fn note_draft_requires_selection() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        // Sem seleção: ignorado, como `annotate_selection`.
        ready.selection = None;
        assert!(!ready.open_note_draft());
        assert!(ready.note_draft.is_none());
        assert!(ready.annotations.is_empty());
        // Com draft aberto, reabrir é no-op (não apaga o texto digitado).
        ready.selection = Some(Selection {
            page: PageNo::first(),
            range: TextRange { start: 0, end: 2 },
        });
        let draft = dummy_draft_with("digitando...");
        ready.note_draft = Some(draft);
        assert!(!ready.open_note_draft());
        assert_eq!(
            note_text(ready.note_draft.as_ref().unwrap()),
            "digitando..."
        );
    }

    #[test]
    fn note_draft_reopens_existing_note_in_editing_mode() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let len = match ready.pages.text.first() {
            Some(Some(layer)) => layer.plain.len(),
            _ => return,
        };
        if len < 2 {
            return;
        }
        // Cria uma nota no trecho 0..2 via save direto.
        ready.note_draft = Some(dummy_draft_with("anotação"));
        assert!(ready.save_note_draft());
        let id = ready.annotations[0].id;
        // N com a seleção sobre o trecho da nota reabre em edição com o texto.
        ready.selection = Some(Selection {
            page: PageNo::first(),
            range: TextRange { start: 0, end: 2 },
        });
        assert!(ready.open_note_draft());
        let draft = ready.note_draft.as_ref().unwrap();
        assert_eq!(draft.editing, Some(id));
        assert_eq!(note_text(draft), "anotação");
        // Clique sobre o marcador (âncora no trecho da nota) idem.
        ready.note_draft = None;
        ready.selection = None;
        assert!(ready.open_note_draft_for_id(id));
        let draft = ready.note_draft.as_ref().unwrap();
        assert_eq!(draft.page, PageNo::first());
        assert_eq!(draft.range, TextRange { start: 0, end: 2 });
        assert_eq!(draft.editing, Some(id));
        assert_eq!(note_text(draft), "anotação");
        // Abrir pelo marcador não sequestra a seleção (o trecho sublinhado
        // não "vem junto"): continua None, como o clique deixou.
        assert!(ready.selection.is_none());
        // Id de marcação que não é nota: falso (o clique remove no handler).
        assert!(!ready.open_note_draft_for_id(u64::MAX));
    }

    /// Tamanho da pilha de undo das marcações: o setup (`note_on_selection`)
    /// já empilha a criação da nota, então os testes comparam o tamanho antes
    /// e depois da interação em vez de assumirem pilha vazia.
    fn undo_len(session: &Session) -> usize {
        match session {
            Session::Ready(ready) => ready.annot_undo.len(),
            _ => unreachable!(),
        }
    }

    /// Tamanho do cartão do post-it nos testes de posição (o da vista).
    const POSTIT_SIZE_TEST: [f32; 2] = [320.0, 200.0];

    /// Texto digitado no rascunho: o editor do iced fecha a última linha com
    /// `\n`, então o que interessa é o que o usuário escreveu.
    fn note_text(draft: &NoteDraft) -> String {
        draft.content.text().trim_end().to_string()
    }

    /// Centro do quad (ponto de página) — onde o clique acerta a marcação.
    fn quad_center(q: Quad) -> [f32; 2] {
        let xs = [q.x0, q.x1, q.x2, q.x3];
        let ys = [q.y0, q.y1, q.y2, q.y3];
        [
            (xs.iter().fold(f32::INFINITY, |a, &b| a.min(b))
                + xs.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b)))
                / 2.0,
            (ys.iter().fold(f32::INFINITY, |a, &b| a.min(b))
                + ys.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b)))
                / 2.0,
        ]
    }

    /// Quad como tupla (o tipo não tem `PartialEq`): comparar posições.
    fn quad_tuple(q: Quad) -> (f32, f32, f32, f32, f32, f32, f32, f32) {
        (q.x0, q.y0, q.x1, q.y1, q.x2, q.y2, q.x3, q.y3)
    }

    /// Quads como tuplas, para comparar ghost e resultado.
    fn quads_tuple(quads: &[Quad]) -> Vec<(f32, f32, f32, f32, f32, f32, f32, f32)> {
        quads.iter().map(|q| quad_tuple(*q)).collect()
    }

    /// Nota real criada pela seleção do trecho 0..2, com o texto dado.
    fn note_on_selection(ready: &mut Ready, text: &str) -> Annotation {
        ready.selection = Some(Selection {
            page: PageNo::first(),
            range: TextRange { start: 0, end: 2 },
        });
        assert!(ready.open_note_draft(), "draft abre com a seleção");
        ready.note_draft.as_mut().unwrap().content = text_editor::Content::with_text(text);
        assert!(ready.save_note_draft(), "save cria a nota");
        ready.annotations.last().cloned().expect("nota criada")
    }

    #[test]
    fn note_delete_from_draft_stacks_remove_and_undo_restores() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let note = note_on_selection(&mut ready, "apagar");
        // Reabre em edição como o clique no marcador e usa o botão vermelho.
        assert!(ready.open_note_draft_for_id(note.id));
        let mut session = Session::Ready(Tabs::single(ready));
        apply(&mut session, Message::NoteDelete);
        match &session {
            Session::Ready(ready) => {
                assert!(ready.note_draft.is_none(), "o post-it fecha ao remover");
                assert!(ready.annotations.is_empty());
                assert!(ready.can_annot_undo(), "remoção entra na pilha");
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::AnnotUndo);
        match &session {
            Session::Ready(ready) => {
                assert_eq!(ready.annotations.len(), 1);
                assert_eq!(ready.annotations[0].id, note.id, "a mesma nota volta");
                assert_eq!(ready.annotations[0].text, "apagar");
                assert_eq!(
                    quads_tuple(&ready.annotations[0].quads),
                    quads_tuple(&note.quads)
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn note_delete_without_editing_keeps_draft() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        // Criando (sem `editing`): o vermelho nem aparece no cartão; o handler
        // não fecha o rascunho nem mexe nas marcações.
        ready.note_draft = Some(dummy_draft_with("nova"));
        assert!(!ready.delete_draft_note());
        assert!(ready.note_draft.is_some());
        assert!(ready.annotations.is_empty());
        assert!(!ready.can_annot_undo());
    }

    #[test]
    fn note_drag_drop_moves_marker_and_keeps_quads() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let note = note_on_selection(&mut ready, "mover");
        let page = note.page;
        let start = note.marker_pt();
        let origin = note.marker_pt();
        let to = [start[0] + 14.0, start[1] - 9.0];
        let want = [origin[0] + 14.0, origin[1] - 9.0];
        let mut session = Session::Ready(Tabs::single(ready));
        let undo_before = undo_len(&session);

        // Press no marcador + arrasto: ghost = só o ícone, na posição
        // candidata; o trecho e o marcador sólido ficam onde estão.
        apply(
            &mut session,
            Message::PointerDown {
                page,
                page_pt: start,
                sheet: [120.0, 60.0],
            },
        );
        apply(&mut session, Message::PointerMove { page, page_pt: to });
        let (ghost_id, ghost_pt) = match &session {
            Session::Ready(ready) => {
                assert_eq!(
                    quads_tuple(&ready.annotations[0].quads),
                    quads_tuple(&note.quads),
                    "durante o arrasto o trecho não se mexe"
                );
                assert_eq!(ready.annotations[0].marker, None, "o sólido não anda");
                ready.note_drag_ghost(page).expect("ghost ativo")
            }
            _ => unreachable!(),
        };
        assert_eq!(ghost_id, note.id);
        assert_eq!(ghost_pt, want, "o ghost anda pelo delta do arrasto");

        // Soltar: o marcador fica exatamente onde o ghost estava; o trecho
        // (quads/range), o texto e o id não mudam.
        apply(&mut session, Message::PointerUp { page, page_pt: to });
        match &session {
            Session::Ready(ready) => {
                assert_eq!(ready.annotations.len(), 1);
                assert_eq!(ready.annotations[0].id, note.id, "id fica");
                assert_eq!(ready.annotations[0].text, "mover", "texto fica");
                assert_eq!(ready.annotations[0].kind, AnnotKind::Note);
                assert_eq!(ready.annotations[0].range, note.range, "trecho fica");
                assert_eq!(
                    quads_tuple(&ready.annotations[0].quads),
                    quads_tuple(&note.quads),
                    "soltar não move a marcação do texto"
                );
                assert_eq!(ready.annotations[0].marker, Some(ghost_pt));
                assert!(ready.note_drag.is_none());
                assert_eq!(
                    ready.annot_undo.len(),
                    undo_before + 2,
                    "soltar empilha Remove(antiga) + Add(movida)"
                );
            }
            _ => unreachable!(),
        }

        // Undo em dois passos (Remove + Add, como o save da edição): volta a
        // nota antiga, com o marcador no trecho (`marker: None`).
        apply(&mut session, Message::AnnotUndo);
        apply(&mut session, Message::AnnotUndo);
        match &session {
            Session::Ready(ready) => {
                assert_eq!(ready.annotations.len(), 1);
                assert_eq!(ready.annotations[0].id, note.id);
                assert_eq!(ready.annotations[0].marker, None);
                assert_eq!(ready.annotations[0].marker_pt(), origin);
                assert_eq!(
                    quads_tuple(&ready.annotations[0].quads),
                    quads_tuple(&note.quads)
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn marker_drag_starts_on_icon_line_press_selects_text() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let note = note_on_selection(&mut ready, "arrastada");
        let page = note.page;
        let line_pt = quad_center(note.quads[0]);
        let media = ready.media(page);
        // Arrasta o marcador para o canto inferior esquerdo da página, longe
        // do trecho marcado — o press parte do ícone, não da linha.
        let press = note.marker_pt();
        let far = [5.0, 5.0];
        let delta = [far[0] - press[0], far[1] - press[1]];
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::PointerDown {
                page,
                page_pt: press,
                sheet: [0.0, 0.0],
            },
        );
        apply(
            &mut session,
            Message::PointerMove {
                page,
                page_pt: [press[0] + delta[0], press[1] + delta[1]],
            },
        );
        apply(
            &mut session,
            Message::PointerUp {
                page,
                page_pt: [press[0] + delta[0], press[1] + delta[1]],
            },
        );
        match &session {
            Session::Ready(ready) => {
                let moved = ready.annotations[0].marker.expect("marcador movido");
                assert_eq!(moved, far);
                assert!(
                    !note.quads.iter().any(|q| q.contains(far[0], far[1])),
                    "o ponto do marcador está fora do trecho"
                );
                // O clique acerta o ícone arrastado; a linha sublinhada não
                // abre mais a nota (o press nela seleciona texto).
                assert_eq!(ready.annotation_at(page, moved), Some(note.id));
                assert_eq!(ready.annotation_at(page, line_pt), None);
                // Um ponto no meio da página, longe de ambos, não acerta nada.
                assert_eq!(
                    ready.annotation_at(page, [media.width * 0.5, media.height * 0.5]),
                    None
                );
            }
            _ => unreachable!(),
        }
        // Press + release sem arrasto em cima da linha sublinhada: ancora
        // seleção de texto, não arrasto de nota — e não abre o post-it.
        apply(
            &mut session,
            Message::PointerDown {
                page,
                page_pt: line_pt,
                sheet: [0.0, 0.0],
            },
        );
        match &session {
            Session::Ready(ready) => {
                assert!(ready.note_drag.is_none(), "linha não inicia arrasto");
                assert!(ready.press_anchor.is_some(), "linha ancora seleção");
            }
            _ => unreachable!(),
        }
        apply(
            &mut session,
            Message::PointerUp {
                page,
                page_pt: line_pt,
            },
        );
        match &session {
            Session::Ready(ready) => {
                assert!(ready.note_draft.is_none(), "linha não abre a nota");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn note_marker_clamped_to_page() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let note = note_on_selection(&mut ready, "borda");
        let page = note.page;
        let start = note.marker_pt();
        let media = ready.media(page);
        let mut session = Session::Ready(Tabs::single(ready));
        // Arrasto muito além do canto: o marcador para na página.
        let out = [start[0] + media.width * 2.0, start[1] - media.height * 2.0];
        apply(
            &mut session,
            Message::PointerDown {
                page,
                page_pt: start,
                sheet: [0.0, 0.0],
            },
        );
        apply(&mut session, Message::PointerMove { page, page_pt: out });
        apply(&mut session, Message::PointerUp { page, page_pt: out });
        match &session {
            Session::Ready(ready) => {
                assert_eq!(
                    ready.annotations[0].marker,
                    Some([media.width, 0.0]),
                    "o marcador para na borda da página"
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn marker_default_none_and_kept_on_edit() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let note = note_on_selection(&mut ready, "original");
        // Nota nova: sem marcador próprio — derivado da origem do 1º quad.
        assert_eq!(note.marker, None);
        let derived = note.marker_pt();
        let q = note.quads[0];
        let left = q.x0.min(q.x1).min(q.x2).min(q.x3);
        let top = q.y0.max(q.y1).max(q.y2).max(q.y3);
        assert_eq!(derived, [left, top], "default = origem do primeiro quad");

        // Editar o texto não move o marcador nem o trecho.
        assert!(ready.open_note_draft_for_id(note.id));
        ready.note_draft.as_mut().unwrap().content = text_editor::Content::with_text("editada");
        assert!(ready.save_note_draft());
        let edited = ready.annotations.last().cloned().expect("nota editada");
        assert_eq!(edited.marker, None, "edição preserva o default");
        assert_eq!(edited.marker_pt(), derived);
        assert_eq!(quads_tuple(&edited.quads), quads_tuple(&note.quads));
        assert_eq!(edited.range, note.range);
        assert_eq!(edited.text, "editada");

        // Com o marcador arrastado, a edição o mantém onde está.
        let marker = Some([derived[0] + 30.0, derived[1] - 20.0]);
        let moved = Annotation {
            marker,
            ..edited.clone()
        };
        ready.annotations = vec![moved];
        assert!(ready.open_note_draft_for_id(edited.id));
        ready.note_draft.as_mut().unwrap().content = text_editor::Content::with_text("de novo");
        assert!(ready.save_note_draft());
        let again = ready.annotations.last().cloned().expect("nota reeditada");
        assert_eq!(again.marker, marker, "edição preserva o marcador arrastado");
        assert_eq!(again.text, "de novo");
        assert_eq!(quads_tuple(&again.quads), quads_tuple(&note.quads));
    }

    #[test]
    fn note_drag_small_press_opens_editor_instead_of_moving() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let note = note_on_selection(&mut ready, "clicar");
        let page = note.page;
        let start = note.marker_pt();
        let mut session = Session::Ready(Tabs::single(ready));
        let undo_before = undo_len(&session);
        // Press + release sem passar do limiar (2 pontos de página ≈ 1 px):
        // é clique — abre a edição e não move nada.
        apply(
            &mut session,
            Message::PointerDown {
                page,
                page_pt: start,
                sheet: [10.0, 10.0],
            },
        );
        apply(
            &mut session,
            Message::PointerMove {
                page,
                page_pt: [start[0] + 2.0, start[1]],
            },
        );
        match &session {
            Session::Ready(ready) => assert!(ready.note_drag_ghost(page).is_none()),
            _ => unreachable!(),
        }
        apply(
            &mut session,
            Message::PointerUp {
                page,
                page_pt: start,
            },
        );
        match &session {
            Session::Ready(ready) => {
                assert_eq!(ready.note_draft.as_ref().unwrap().editing, Some(note.id));
                assert_eq!(
                    quads_tuple(&ready.annotations[0].quads),
                    quads_tuple(&note.quads)
                );
                assert_eq!(ready.annotations[0].marker, None, "clique não move");
                assert_eq!(ready.annot_undo.len(), undo_before, "clique não empilha");
                assert!(ready.selection.is_none(), "clique não seleciona o trecho");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn note_drag_cancel_paths_leave_note_untouched() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let note = note_on_selection(&mut ready, "cancelar");
        let page = note.page;
        let start = note.marker_pt();
        let to = [start[0] + 40.0, start[1] + 25.0];

        // Soltar fora da folha (a vista manda DragCancelled): nada muda.
        let mut session = Session::Ready(Tabs::single(ready));
        let undo_before = undo_len(&session);
        apply(
            &mut session,
            Message::PointerDown {
                page,
                page_pt: start,
                sheet: [0.0, 0.0],
            },
        );
        apply(&mut session, Message::PointerMove { page, page_pt: to });
        apply(&mut session, Message::DragCancelled);
        match &session {
            Session::Ready(ready) => {
                assert!(ready.note_drag.is_none());
                assert!(ready.note_drag_ghost(page).is_none());
                assert_eq!(
                    quads_tuple(&ready.annotations[0].quads),
                    quads_tuple(&note.quads)
                );
                assert_eq!(ready.annotations[0].marker, None, "cancelar não move");
                assert_eq!(ready.annot_undo.len(), undo_before, "cancelar não empilha");
            }
            _ => unreachable!(),
        }

        // Esc no meio do arrasto: idem (o mesmo canal fecha o post-it).
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let note = note_on_selection(&mut ready, "cancelar");
        let mut session = Session::Ready(Tabs::single(ready));
        let undo_before = undo_len(&session);
        apply(
            &mut session,
            Message::PointerDown {
                page,
                page_pt: start,
                sheet: [0.0, 0.0],
            },
        );
        apply(&mut session, Message::PointerMove { page, page_pt: to });
        apply(&mut session, Message::ClosePrintDialog);
        match &session {
            Session::Ready(ready) => {
                assert!(ready.note_drag.is_none());
                assert_eq!(
                    quads_tuple(&ready.annotations[0].quads),
                    quads_tuple(&note.quads)
                );
                assert_eq!(ready.annotations[0].marker, None, "Esc não move");
                assert_eq!(ready.annot_undo.len(), undo_before, "Esc não empilha");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn postit_position_is_clamped_to_window() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        ready.viewport = Viewport {
            width: 800.0,
            height: 600.0,
        };
        ready.note_draft = Some(dummy_draft());
        let size = [320.0, 200.0];
        // Âncora no canto (folha em 0,0): a margem segura o post-it.
        assert_eq!(ready.postit_pos(size), [POSTIT_MARGIN, POSTIT_MARGIN]);
        // Nota no fim da página: preso dentro da janela, sem vazar a borda.
        let draft = ready.note_draft.as_mut().unwrap();
        draft.anchor = [5000.0, 5000.0];
        assert_eq!(
            ready.postit_pos(size),
            [800.0 - 320.0 - POSTIT_MARGIN, 600.0 - 200.0 - POSTIT_MARGIN]
        );
        // Janela menor que o post-it: ainda dentro (margem dos dois lados).
        assert_eq!(
            ready.postit_pos([2000.0, 2000.0]),
            [POSTIT_MARGIN, POSTIT_MARGIN]
        );
    }

    #[test]
    fn postit_follows_document_scroll() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let note = note_on_selection(&mut ready, "rolar");
        let page = note.page;
        let start = note.marker_pt();
        let mut session = Session::Ready(Tabs::single(ready));
        // Clique no marcador com a folha em (120, 60): abre o post-it.
        apply(
            &mut session,
            Message::PointerDown {
                page,
                page_pt: start,
                sheet: [120.0, 60.0],
            },
        );
        apply(
            &mut session,
            Message::PointerUp {
                page,
                page_pt: start,
            },
        );
        let before = match &session {
            Session::Ready(ready) => {
                let draft = ready.note_draft.as_ref().expect("post-it aberto");
                assert!(
                    draft.anchor[0] > 120.0 && draft.anchor[1] >= 60.0,
                    "nasce ancorado dentro da folha: {:?}",
                    draft.anchor
                );
                ready.postit_pos(POSTIT_SIZE_TEST)
            }
            _ => unreachable!(),
        };
        // Rolar o documento sobe a folha: o post-it sobe junto (mesmo delta).
        apply(&mut session, Message::DocScrolled(30.0));
        match &session {
            Session::Ready(ready) => {
                assert_eq!(ready.postit_pos(POSTIT_SIZE_TEST)[1], before[1] - 30.0);
            }
            _ => unreachable!(),
        }
    }

    fn glyph_at(cluster: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Glyph {
        Glyph {
            cluster: cluster.to_string(),
            quad: Quad::from_rect(x0, y0, x1, y1),
        }
    }

    #[test]
    fn quads_by_line_split_lines_and_columns() {
        // Duas linhas + segunda coluna na mesma faixa: 3 retângulos, sem vão.
        let glyphs = vec![
            glyph_at("a", 0.0, 0.0, 10.0, 10.0),
            glyph_at("b", 10.0, 0.0, 20.0, 10.0),
            glyph_at("c", 0.0, 20.0, 10.0, 30.0),
            glyph_at("d", -20.0, 20.0, -10.0, 30.0),
        ];
        let quads = quads_for_range_by_line(&glyphs, 0, 4);
        assert_eq!(quads.len(), 3);
        // Linha 1 cobre só a primeira faixa (sem vazar para a de baixo).
        let top = quad_bbox(quads[0]);
        assert_eq!((top.0, top.1, top.2, top.3), (0.0, 0.0, 20.0, 10.0));
        // Range vazio: nada.
        assert!(quads_for_range_by_line(&glyphs, 4, 4).is_empty());
    }

    #[test]
    fn drag_back_shrinks_to_anchor() {
        let anchor = Some(TextRange { start: 10, end: 20 });
        // Para frente: estende o fim.
        assert_eq!(
            extend_range(anchor, (15, 25)),
            TextRange { start: 10, end: 25 }
        );
        // Para trás: estende o início.
        assert_eq!(
            extend_range(anchor, (0, 5)),
            TextRange { start: 0, end: 20 }
        );
        // De volta para dentro: encolhe até a âncora (não menos).
        assert_eq!(
            extend_range(anchor, (12, 13)),
            TextRange { start: 10, end: 20 }
        );
        // Sem âncora: ancora no cursor.
        assert_eq!(extend_range(None, (4, 9)), TextRange { start: 4, end: 9 });
    }

    #[test]
    fn pointer_down_miss_and_escape_clear_selection() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        ready.selection = Some(Selection {
            page: PageNo::first(),
            range: TextRange { start: 0, end: 2 },
        });
        let mut session = Session::Ready(Tabs::single(ready));
        // Press longe de qualquer glifo: ancora no mais próximo (snap).
        apply(
            &mut session,
            Message::PointerDown {
                page: PageNo::first(),
                page_pt: [-1000.0, -1000.0],
                sheet: [0.0, 0.0],
            },
        );
        match &session {
            Session::Ready(ready) => assert!(ready.selection.is_some()),
            _ => unreachable!(),
        }
        // ...mas soltar sem arrastar no vazio limpa (clique no vazio).
        apply(
            &mut session,
            Message::PointerUp {
                page: PageNo::first(),
                page_pt: [-1000.0, -1000.0],
            },
        );
        match &session {
            Session::Ready(ready) => assert!(ready.selection.is_none()),
            _ => unreachable!(),
        }
        // Esc sem diálogo também limpa.
        match &mut session {
            Session::Ready(ready) => {
                ready.selection = Some(Selection {
                    page: PageNo::first(),
                    range: TextRange { start: 0, end: 2 },
                });
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::ClosePrintDialog);
        match &session {
            Session::Ready(ready) => assert!(ready.selection.is_none()),
            _ => unreachable!(),
        }
    }

    #[test]
    fn drag_from_blank_extends_from_snapped_anchor() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        ready.selection = None;
        let mut session = Session::Ready(Tabs::single(ready));
        // Press no vazio ancora no glifo mais próximo...
        apply(
            &mut session,
            Message::PointerDown {
                page: PageNo::first(),
                page_pt: [-1000.0, -1000.0],
                sheet: [0.0, 0.0],
            },
        );
        // ...e arrastar dali estende a seleção (não fica travada).
        apply(
            &mut session,
            Message::PointerMove {
                page: PageNo::first(),
                page_pt: [9000.0, 9000.0],
            },
        );
        match &session {
            Session::Ready(ready) => {
                let sel = ready.selection.clone().expect("snap ancora");
                assert!(sel.range.end > sel.range.start, "{sel:?}");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn navigate_persists_position_for_real_file() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let file =
            std::env::temp_dir().join(format!("tsuro-positions-unit-{}-nav", std::process::id()));
        let _ = std::fs::remove_file(&file);
        crate::positions::with_positions_path(file.clone(), || {
            let last = ready.page_count().saturating_sub(1);
            let mut session = Session::Ready(Tabs::single(ready));
            apply(
                &mut session,
                Message::Nav(NavCmd::GoTo(PageNo::from_index(last))),
            );
            let back = crate::positions::read_positions();
            assert_eq!(back.len(), 1);
            assert_eq!(back[0].1.page, last);
        });
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn restore_position_applies_stored_page_and_resets_history() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let file = std::env::temp_dir().join(format!(
            "tsuro-positions-unit-{}-restore",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file);
        crate::positions::with_positions_path(file.clone(), || {
            let path = ready.source.path().to_path_buf();
            let (size, mtime) = crate::positions::file_identity(&path).unwrap();
            let last = ready.page_count().saturating_sub(1);
            let entries = crate::positions::record_position(
                Vec::new(),
                path,
                crate::positions::DocPosition {
                    page: last,
                    zoom: Zoom::Page,
                    mode: ViewMode::Continuous,
                    size,
                    mtime,
                },
            );
            crate::positions::save_positions(&entries).unwrap();
            ready.restore_position();
            assert_eq!(ready.visible.index(), last);
            assert!(matches!(ready.zoom, Zoom::Page));
            assert_eq!(ready.view_mode, ViewMode::Continuous);
            // Contínuo restaurado já abre na página certa (janela + scroll).
            assert_eq!(ready.doc_scroll_y, ready.page_offset(ready.visible));
            assert_eq!(ready.history.pages, vec![ready.visible]);
            assert_eq!(ready.history.pos, 0);
        });
        let _ = std::fs::remove_file(&file);
    }

    /// Smoke de estado (fixture real): navegar até a última página, fechar e
    /// reabrir pelo caminho de produção (`begin_open`+`finish_open`) volta pra lá,
    /// com zoom e modo persistidos.
    #[test]
    fn reopen_restores_page_zoom_and_mode_after_close() {
        isolated(|| {
            let Some(ready) = sample_ready() else {
                return;
            };
            let last = ready.page_count().saturating_sub(1);
            if last < 1 {
                return;
            }
            let path = ready.source.path().to_path_buf();
            let file = std::env::temp_dir().join(format!(
                "tsuro-positions-unit-{}-reopen",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&file);
            crate::positions::with_positions_path(file.clone(), || {
                let target = PageNo::from_index(last);
                let mut session = Session::Ready(Tabs::single(ready));
                apply(&mut session, Message::Nav(NavCmd::GoTo(target)));
                apply(&mut session, Message::SetZoom(Zoom::Page));
                apply(&mut session, Message::SetViewMode(ViewMode::Continuous));
                apply(&mut session, Message::Close);
                let Some(reopened) = sample_ready() else {
                    return;
                };
                let _ = session.begin_open(OpenSource::Path(path.clone()));
                session.finish_open(Ok(reopened));
                match &session {
                    Session::Ready(ready) => {
                        assert_eq!(ready.visible, target);
                        assert!(matches!(ready.zoom, Zoom::Page));
                        assert_eq!(ready.view_mode, ViewMode::Continuous);
                        assert_eq!(ready.doc_scroll_y, ready.page_offset(ready.visible));
                        // Restaurar não gera entrada: a pilha reinicia aqui.
                        assert_eq!(ready.history.pages, vec![target]);
                        assert_eq!(ready.history.pos, 0);
                        assert!(!ready.can_history_back());
                    }
                    other => panic!("expected Ready, got {other:?}"),
                }
            });
            let _ = std::fs::remove_file(&file);
        });
    }

    #[test]
    fn outline_rows_flatten_collapse_and_active() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        assert!(ready.outline_rows().is_empty());
        assert!(ready.outline_active().is_none());
        ready.outline = Some(outline_tree());
        let rows = ready.outline_rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].0, vec![0]);
        assert_eq!(rows[1].0, vec![0, 0]);
        assert!(rows[0].4);
        assert!(!rows[1].4);
        // Colapsar esconde os filhos.
        ready.outline_collapsed.insert(vec![0]);
        let rows = ready.outline_rows();
        assert_eq!(rows.len(), 2);
        ready.outline_collapsed.clear();
        // Ativa = último item com página <= visible.
        ready.visible = PageNo::from_index(3);
        assert_eq!(ready.outline_active(), Some(vec![0, 0]));
        ready.visible = PageNo::from_index(8);
        assert_eq!(ready.outline_active(), Some(vec![1]));
        ready.visible = PageNo::first();
        assert_eq!(ready.outline_active(), Some(vec![0]));
    }

    #[test]
    fn outline_keyboard_walks_tree_and_jumps() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        let doc_gen = active_ready(&session).open_gen;
        apply(
            &mut session,
            Message::OutlineLoaded {
                doc_gen,
                outline: Some(outline_tree()),
            },
        );
        // Sem a aba aberta as teclas não andam nem saltam.
        apply(&mut session, Message::OutlineKey(OutlineKey::Next));
        apply(&mut session, Message::OutlineKey(OutlineKey::Activate));
        match &session {
            Session::Ready(r) => {
                assert!(r.outline_cursor.is_none());
                assert_eq!(r.visible, PageNo::first());
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::OutlineTab(true));
        // O cursor parte da entrada ativa (página 1 → primeiro item).
        match &session {
            Session::Ready(r) => assert_eq!(r.outline_focus(), Some(vec![0])),
            _ => unreachable!(),
        }
        // ↓ anda pela árvore sem navegar: quem salta é o Enter.
        apply(&mut session, Message::OutlineKey(OutlineKey::Next));
        match &session {
            Session::Ready(r) => {
                assert_eq!(r.outline_focus(), Some(vec![0, 0]));
                assert_eq!(r.visible, PageNo::first());
            }
            _ => unreachable!(),
        }
        // Enter salta para a página do cursor e entra no histórico.
        let history = match &session {
            Session::Ready(r) => r.history.len(),
            _ => unreachable!(),
        };
        apply(&mut session, Message::OutlineKey(OutlineKey::Activate));
        match &session {
            Session::Ready(r) => {
                // Alvo é a página do cursor, com o mesmo clamp do SetPage
                // (o fixture tem 2 páginas, então a página 3 cai na última).
                let last = PageNo::from_index(r.page_count() - 1);
                assert_eq!(r.visible, PageNo::from_index(2).min(last));
                assert_eq!(r.history.len(), history + 1);
            }
            _ => unreachable!(),
        }
        // ↑/↓ não passam das pontas da lista.
        for _ in 0..4 {
            apply(&mut session, Message::OutlineKey(OutlineKey::Next));
        }
        match &session {
            Session::Ready(r) => assert_eq!(r.outline_focus(), Some(vec![1])),
            _ => unreachable!(),
        }
        for _ in 0..4 {
            apply(&mut session, Message::OutlineKey(OutlineKey::Prev));
        }
        match &session {
            Session::Ready(r) => assert_eq!(r.outline_focus(), Some(vec![0])),
            _ => unreachable!(),
        }
        // Colapsar o nó esconde os filhos: o cursor volta para a ativa.
        apply(&mut session, Message::OutlineKey(OutlineKey::Next));
        apply(&mut session, Message::OutlineFold(vec![0]));
        match &session {
            Session::Ready(r) => {
                assert_eq!(r.outline_rows().len(), 2);
                assert_eq!(r.outline_focus(), Some(vec![0]));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn outline_keys_map_to_outline_messages() {
        use iced::event::Status;
        use iced::keyboard::Modifiers;
        let plain = Modifiers::empty();
        assert!(matches!(
            keyboard_message(Key::Named(Named::ArrowUp), plain, Status::Ignored),
            Some(Message::OutlineKey(OutlineKey::Prev))
        ));
        assert!(matches!(
            keyboard_message(Key::Named(Named::ArrowDown), plain, Status::Ignored),
            Some(Message::OutlineKey(OutlineKey::Next))
        ));
        assert!(matches!(
            keyboard_message(Key::Named(Named::Enter), plain, Status::Ignored),
            Some(Message::OutlineKey(OutlineKey::Activate))
        ));
        // Com modificador a tecla sai do mapa (⌘/Alt+setas seguem no histórico)
        // e com foco em campo o evento chega capturado: nada é despachado.
        assert!(keyboard_message(
            Key::Named(Named::ArrowUp),
            Modifiers::SHIFT,
            Status::Ignored
        )
        .is_none());
        assert!(keyboard_message(Key::Named(Named::Enter), plain, Status::Captured).is_none());
    }

    #[test]
    fn outline_tab_jump_fold_and_load() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let last = ready.page_count().saturating_sub(1);
        let mut session = Session::Ready(Tabs::single(ready));
        // Sem outline, a aba não abre.
        apply(&mut session, Message::OutlineTab(true));
        match &session {
            Session::Ready(r) => assert!(!r.outline_open),
            _ => unreachable!(),
        }
        // Com outline, abre; jump com clamp; fold alterna.
        let doc_gen = active_ready(&session).open_gen;
        apply(
            &mut session,
            Message::OutlineLoaded {
                doc_gen,
                outline: Some(outline_tree()),
            },
        );
        apply(&mut session, Message::OutlineTab(true));
        apply(&mut session, Message::OutlineJump(PageNo::from_index(9999)));
        match &session {
            Session::Ready(r) => {
                assert!(r.outline_open);
                assert_eq!(r.visible.index(), last);
                let expected = r
                    .outline_rows()
                    .into_iter()
                    .filter(|(_, _, _, page, _)| page.index() <= last)
                    .map(|(path, _, _, _, _)| path)
                    .last();
                assert_eq!(r.outline_active(), expected);
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::OutlineFold(vec![0]));
        match &session {
            Session::Ready(r) => {
                assert!(r.outline_collapsed.contains(&vec![0]));
                assert_eq!(r.outline_rows().len(), 2);
            }
            _ => unreachable!(),
        }
        apply(&mut session, Message::OutlineFold(vec![0]));
        match &session {
            Session::Ready(r) => assert!(r.outline_collapsed.is_empty()),
            _ => unreachable!(),
        }
    }

    #[test]
    fn close_and_reopen_do_not_inherit_panel_flags() {
        isolated(|| {
            let Some(ready) = sample_ready() else {
                return;
            };
            let path = ready.source.path().to_path_buf();
            let mut session = Session::Ready(Tabs::single(ready));
            apply(&mut session, Message::ToggleSignatures);
            apply(&mut session, Message::TogglePages);
            apply(&mut session, Message::Close);
            assert!(matches!(session, Session::Empty(_)));
            let Some(ready) = sample_ready() else {
                return;
            };
            let _ = session.begin_open(OpenSource::Path(path));
            session.finish_open(Ok(ready));
            match &session {
                Session::Ready(ready) => {
                    assert!(!ready.signatures_open);
                    assert!(!ready.pages_open);
                }
                other => panic!("expected Ready, got {other:?}"),
            }
        });
    }

    #[test]
    fn finish_open_merges_disk_recents_when_memory_is_empty() {
        isolated(|| {
            save_recents(&[PathBuf::from("/tmp/disk.pdf")]).unwrap();
            let mut session = Session::Empty(EmptyState::default());
            let _ = session.begin_open(OpenSource::Path(PathBuf::from("/tmp/new.pdf")));
            session.finish_open(Err(OpenError::Engine("sem motor no teste".into())));
            match &session {
                Session::Failed { recents, .. } => {
                    assert!(recents.contains(&PathBuf::from("/tmp/disk.pdf")));
                }
                other => panic!("expected Failed, got {other:?}"),
            }
        });
    }

    #[test]
    fn recents_ready_fills_loading_session() {
        let mut session = Session::Loading {
            source: OpenSource::Path(PathBuf::from("/tmp/direct.pdf")),
            recents: Vec::new(),
            gen: 1,
            theme: Theme::Dark,
            render_scale: 1.0,
            phase: 0,
        };
        apply(
            &mut session,
            Message::RecentsReady(vec![PathBuf::from("/tmp/old.pdf")]),
        );
        match &session {
            Session::Loading { recents, .. } => {
                assert_eq!(recents, &vec![PathBuf::from("/tmp/old.pdf")]);
            }
            other => panic!("expected Loading, got {other:?}"),
        }
    }

    #[test]
    fn loading_tick_advances_and_wraps_the_bar_phase() {
        isolated(|| {
            let mut session = Session::open_path(PathBuf::from("/tmp/direct.pdf"));
            apply(&mut session, Message::LoadingTick);
            apply(&mut session, Message::LoadingTick);
            match &session {
                Session::Loading { phase, .. } => assert_eq!(*phase, 2),
                other => panic!("expected Loading, got {other:?}"),
            }
            // Abertura longa (u16::MAX tiques ≈ 1,6 h): `wrapping_add` não estoura.
            if let Session::Loading { phase, .. } = &mut session {
                *phase = u16::MAX;
            }
            apply(&mut session, Message::LoadingTick);
            match &session {
                Session::Loading { phase, .. } => assert_eq!(*phase, 0),
                other => panic!("expected Loading, got {other:?}"),
            }
        });
    }

    #[test]
    fn stale_opened_after_close_is_ignored() {
        isolated(|| {
            let mut session = Session::empty();
            let _ = session.begin_open(OpenSource::Path(PathBuf::from("/tmp/a.pdf")));
            let gen = match &session {
                Session::Loading { gen, .. } => *gen,
                other => panic!("expected Loading, got {other:?}"),
            };
            apply(&mut session, Message::Close);
            assert!(matches!(session, Session::Empty(_)));
            apply(
                &mut session,
                Message::Opened {
                    gen,
                    result: Err(OpenError::Engine("atrasado".into())),
                },
            );
            match &session {
                Session::Empty(_) => {}
                other => panic!("stale Opened must not leave Empty, got {other:?}"),
            }
        });
    }

    #[test]
    fn stale_opened_does_not_replace_newer_open() {
        isolated(|| {
            let mut session = Session::empty();
            let _ = session.begin_open(OpenSource::Path(PathBuf::from("/tmp/a.pdf")));
            let gen_a = match &session {
                Session::Loading { gen, .. } => *gen,
                other => panic!("expected Loading, got {other:?}"),
            };
            let _ = session.begin_open(OpenSource::Path(PathBuf::from("/tmp/b.pdf")));
            apply(
                &mut session,
                Message::Opened {
                    gen: gen_a,
                    result: Err(OpenError::Engine("A atrasado".into())),
                },
            );
            match &session {
                Session::Loading { source, .. } => {
                    assert_eq!(source.path(), PathBuf::from("/tmp/b.pdf").as_path());
                }
                other => panic!("A's result replaced B, got {other:?}"),
            }
        });
    }

    #[test]
    fn open_recent_ignores_non_pdf() {
        let mut session = Session::empty();
        apply(
            &mut session,
            Message::OpenRecent(PathBuf::from("/tmp/note.txt")),
        );
        assert!(matches!(session, Session::Empty(_)));
    }

    #[test]
    fn failed_render_is_not_cached() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let page = ready.visible;
        let scale = ready.page_scale(page);
        let doc_gen = ready.open_gen;
        let render_gen = ready.render_gen;
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::Rendered {
                page,
                scale,
                rotation: 0,
                doc_gen,
                render_gen,
                surface: None,
            },
        );
        match &session {
            Session::Ready(ready) => {
                assert!(ready.surface(page, scale).is_none());
                assert!(ready.visible_render_failed());
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        apply(
            &mut session,
            Message::SetViewport(Viewport {
                width: 960.0,
                height: 720.0,
            }),
        );
        match &session {
            Session::Ready(ready) => {
                assert!(ready.surface(page, scale).is_none());
                assert!(ready.visible_render_failed());
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    impl std::fmt::Debug for Session {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Session::Empty(empty) => f.debug_tuple("Empty").field(empty).finish(),
                Session::Loading { source, .. } => {
                    f.debug_struct("Loading").field("source", source).finish()
                }
                Session::Ready(ready) => f.debug_tuple("Ready").field(ready).finish(),
                Session::Failed { message, .. } => {
                    f.debug_struct("Failed").field("message", message).finish()
                }
            }
        }
    }

    fn sample_pdf() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../public/samples/guia-folio.pdf")
    }

    #[test]
    fn page_submit_clamps_and_restores_invalid() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let total = ready.page_count();
        ready.page_input = "9999".into();
        ready.submit_page_input();
        assert_eq!(ready.visible.index(), total - 1);
        assert_eq!(ready.page_input(), total.to_string());

        ready.page_input = "abc".into();
        ready.submit_page_input();
        assert_eq!(ready.page_input(), total.to_string());

        ready.page_input = "0".into();
        ready.submit_page_input();
        assert_eq!(ready.visible.index(), 0);
        assert_eq!(ready.page_input(), "1");

        ready.page_input = "  ".into();
        ready.submit_page_input();
        assert_eq!(ready.page_input(), "1");
    }

    #[test]
    fn page_submit_valid_and_external_nav_sync() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        if ready.page_count() < 3 {
            return;
        }
        ready.page_input = "2".into();
        ready.submit_page_input();
        assert_eq!(ready.visible.index(), 1);
        assert_eq!(ready.page_input(), "2");

        ready.navigate_to(PageNo::from_index(0));
        assert_eq!(ready.page_input(), "1");
    }

    #[test]
    fn keyboard_message_respects_focus_and_modifiers() {
        use iced::event::Status;
        use iced::keyboard::Modifiers;
        match keyboard_message(
            Key::Named(Named::PageDown),
            Modifiers::default(),
            Status::Ignored,
        ) {
            Some(Message::Nav(NavCmd::Next)) => {}
            other => panic!("expected PageDown -> Next, got {other:?}"),
        }
        match keyboard_message(
            Key::Named(Named::ArrowLeft),
            Modifiers::default(),
            Status::Ignored,
        ) {
            Some(Message::Nav(NavCmd::Previous)) => {}
            other => panic!("expected ArrowLeft -> Previous, got {other:?}"),
        }
        match keyboard_message(
            Key::Named(Named::ArrowRight),
            Modifiers::default(),
            Status::Ignored,
        ) {
            Some(Message::Nav(NavCmd::Next)) => {}
            other => panic!("expected ArrowRight -> Next, got {other:?}"),
        }
        assert!(keyboard_message(
            Key::Named(Named::Home),
            Modifiers::default(),
            Status::Captured
        )
        .is_none());
        assert!(
            keyboard_message(Key::Named(Named::End), Modifiers::SHIFT, Status::Ignored).is_none()
        );
    }

    #[test]
    fn rotate_view_cycles_quarters_and_resets_on_open() {
        let Some(ready) = sample_ready() else {
            return;
        };
        assert_eq!(ready.view_rotation, 0);
        let mut session = Session::Ready(Tabs::single(ready));
        for expected in [1, 2, 3, 0] {
            apply(&mut session, Message::RotateView);
            let Session::Ready(r) = &session else {
                panic!("expected Ready");
            };
            assert_eq!(r.view_rotation, expected);
        }
        // Abrir outro documento não descarta mais a sessão (issue #40): o novo
        // entra como aba e cada uma guarda a sua rotação — a nova nasce em 0.
        apply(&mut session, Message::RotateView);
        let second = sample_ready();
        let _ = session.begin_open(OpenSource::Path(sample_pdf()));
        let Session::Ready(tabs) = &session else {
            panic!("a aba atual continua na tela durante o carregamento");
        };
        assert_eq!(tabs.active().view_rotation, 1);
        assert_eq!(tabs.len(), 1, "a aba só entra quando o arquivo chega");
        let Some(second) = second else {
            return;
        };
        session.finish_open(Ok(second));
        let Session::Ready(tabs) = &session else {
            panic!("expected Ready");
        };
        assert_eq!(tabs.len(), 2);
        assert_eq!(
            tabs.active().view_rotation,
            0,
            "a aba nova nasce sem rotação"
        );
        assert_eq!(
            tabs.docs()[0].view_rotation,
            1,
            "a aba de origem fica como está"
        );
    }

    #[test]
    fn fit_uses_rotated_media() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        ready.viewport = Viewport {
            width: 800.0,
            height: 600.0,
        };
        ready.zoom = Zoom::Page;
        let page = ready.visible;
        let media = ready.media(page);
        ready.view_rotation = 1;
        let swapped = MediaBox {
            width: media.height,
            height: media.width,
        };
        // Ajuste à página cabe na moldura útil (não na janela cheia).
        let avail_w = 800.0 - CHROME_PAD - 2.0 * DOC_PAD_X;
        let avail_h = 600.0 - CHROME_PAD;
        let expect = (avail_w / swapped.width).min(avail_h / swapped.height);
        assert!((ready.page_scale(page).factor() - expect).abs() < 0.002);
    }

    #[test]
    fn zoom_step_follows_rotated_fit() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        ready.viewport = Viewport {
            width: 800.0,
            height: 600.0,
        };
        ready.zoom = Zoom::Page;
        let page = ready.visible;
        let media = ready.media(page);
        let upright = ready.zoom_step_factor();
        ready.view_rotation = 1;
        let rotated = ready.rotated_media(page);
        let avail_w = 800.0 - CHROME_PAD - 2.0 * DOC_PAD_X;
        let avail_h = 600.0 - CHROME_PAD;
        let expect = (avail_w / rotated.width).min(avail_h / rotated.height);
        assert!((ready.zoom_step_factor() - expect).abs() < 0.001);
        // Página não quadrada: girar muda o ajuste, então o passo de +/− tem
        // de partir do fator girado (antes partia do original e o + encolhia).
        if media.width != media.height {
            assert_ne!(ready.zoom_step_factor(), upright);
        }
    }

    /// Spec rotação (smoke): com a vista girada, clicar na geometria
    /// exibida de um glifo seleciona esse glifo e o quad pintado cobre o
    /// ponto clicado — a seleção acompanha a página girada.
    #[test]
    fn click_on_rotated_page_selects_the_clicked_glyph() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        ready.viewport = Viewport {
            width: 900.0,
            height: 700.0,
        };
        ready.zoom = Zoom::Page;
        let page = PageNo::first();
        ready.visible = page;
        let media = ready.media(page);
        let Some(layer) = ready.pages.text[page.index() as usize].clone() else {
            return;
        };
        // Glifo com texto real (espaço não gera seleção), do meio da página.
        let middle = layer.glyphs.len() / 2;
        let Some(k) = (middle..layer.glyphs.len()).chain(0..middle).find(|&i| {
            let (start, end) = glyph_byte_range(&layer, i);
            !layer.slice(TextRange { start, end }).trim().is_empty()
        }) else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        for step in 0..4u8 {
            if step > 0 {
                apply(&mut session, Message::RotateView);
            }
            let Session::Ready(ready) = &session else {
                panic!("expected Ready");
            };
            assert_eq!(ready.view_rotation, step);
            let rotated = ready.rotated_media(page);
            let (dw, dh) = (900.0, 900.0 * rotated.height / rotated.width.max(1.0));
            let [x, y, w, h] = display_rect(layer.glyphs[k].quad, media, step, dw, dh);
            let at = [x + w / 2.0, y + h / 2.0];
            let page_pt = page_pt_at(at, media, step, dw, dh);
            apply(
                &mut session,
                Message::PointerDown {
                    page,
                    page_pt,
                    sheet: [0.0, 0.0],
                },
            );
            let Session::Ready(ready) = &session else {
                panic!("expected Ready");
            };
            let (sel_page, quads) = ready
                .selection_quads()
                .unwrap_or_else(|| panic!("rotação {step}: clique no texto não selecionou"));
            assert!(
                quads.iter().any(|q| {
                    let [qx, qy, qw, qh] = display_rect(*q, media, step, dw, dh);
                    (qx - 1.0..=qx + qw + 1.0).contains(&at[0])
                        && (qy - 1.0..=qy + qh + 1.0).contains(&at[1])
                }),
                "rotação {step}: quad pintado não cobriu o clique"
            );
            apply(&mut session, Message::PointerUp { page, page_pt });
        }
    }

    #[test]
    fn rotated_render_swaps_bitmap_dims() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let page = PageNo::first();
        let scale = Scale::from_factor(1.0);
        let plain = ready.engine.render(page, scale, 0).expect("render");
        let rotated = ready.engine.render(page, scale, 1).expect("rotated render");
        assert_eq!(plain.bitmap.width, rotated.bitmap.height);
        assert_eq!(plain.bitmap.height, rotated.bitmap.width);
    }

    #[test]
    fn render_key_separates_rotations() {
        let page = PageNo::first();
        let scale = Scale::from_factor(1.0);
        assert_ne!(render_key(page, scale, 0), render_key(page, scale, 1));
        assert_eq!(render_key(page, scale, 5), render_key(page, scale, 1));
    }

    #[test]
    fn keyboard_r_rotates_view_with_focus_guard() {
        use iced::event::Status;
        use iced::keyboard::Modifiers;
        match keyboard_message(
            Key::Character("r".into()),
            Modifiers::default(),
            Status::Ignored,
        ) {
            Some(Message::RotateView) => {}
            other => panic!("expected r -> RotateView, got {other:?}"),
        }
        assert!(keyboard_message(
            Key::Character("r".into()),
            Modifiers::SHIFT,
            Status::Ignored,
        )
        .is_none());
        assert!(keyboard_message(
            Key::Character("r".into()),
            Modifiers::default(),
            Status::Captured,
        )
        .is_none());
    }

    #[test]
    fn stale_render_gen_is_ignored() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let page = ready.visible;
        let scale = ready.page_scale(page);
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::Rendered {
                page,
                scale,
                rotation: 0,
                doc_gen: 1,
                render_gen: 0,
                surface: Some(fake_surface(page, scale)),
            },
        );
        let Session::Ready(ready) = &session else {
            panic!("expected Ready");
        };
        assert!(ready.surface(page, scale).is_none());
    }

    #[test]
    fn cache_replaces_scale_and_shows_stale_until_exact() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        let page = ready.visible;
        let old_scale = Scale::from_factor(1.0);
        let new_scale = Scale::from_factor(2.0);
        ready
            .surfaces
            .insert(page, old_scale, 0, fake_surface(page, old_scale));
        assert!(ready.surface(page, old_scale).is_some());
        assert!(ready.visible_surface().is_some());
        ready
            .surfaces
            .insert(page, new_scale, 0, fake_surface(page, new_scale));
        assert!(ready.surface(page, new_scale).is_some());
        assert!(ready.surface(page, old_scale).is_none());
    }

    #[test]
    fn evict_retains_visible_neighbors() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        if ready.page_count() < 3 {
            return;
        }
        let s0 = Scale::from_factor(1.0);
        ready.visible = PageNo::from_index(1);
        ready.surfaces.insert(
            PageNo::from_index(0),
            s0,
            0,
            fake_surface(PageNo::from_index(0), s0),
        );
        ready.surfaces.insert(
            PageNo::from_index(1),
            s0,
            0,
            fake_surface(PageNo::from_index(1), s0),
        );
        ready.surfaces.insert(
            PageNo::from_index(2),
            s0,
            0,
            fake_surface(PageNo::from_index(2), s0),
        );
        ready.evict_unused();
        assert!(ready.surface(PageNo::from_index(0), s0).is_some());
        assert!(ready.surface(PageNo::from_index(1), s0).is_some());
        assert!(ready.surface(PageNo::from_index(2), s0).is_some());
    }

    #[test]
    fn search_many_unicode_matches_share_glyph_walk() {
        let mut glyphs = Vec::new();
        let mut plain = String::new();
        for _ in 0..50 {
            plain.push_str("ação ");
            glyphs.push(Glyph {
                cluster: "ação ".into(),
                quad: Quad::from_rect(0.0, 0.0, 10.0, 10.0),
            });
        }
        let layer = TextLayer {
            page: PageNo::first(),
            plain,
            glyphs,
        };
        let hits = find_hits("ação", &layer);
        assert_eq!(hits.len(), 50);
    }

    #[test]
    fn stale_doc_gen_render_is_ignored() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let page = ready.visible;
        let scale = ready.page_scale(page);
        let stale_doc = ready.open_gen.wrapping_add(1);
        let render_gen = ready.render_gen;
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::Rendered {
                page,
                scale,
                rotation: 0,
                doc_gen: stale_doc,
                render_gen,
                surface: Some(fake_surface(page, scale)),
            },
        );
        let Session::Ready(ready) = &session else {
            panic!("expected Ready");
        };
        assert!(ready.surface(page, scale).is_none());
    }

    #[test]
    fn page_data_failure_is_not_retried() {
        let Some(ready) = sample_ready() else {
            return;
        };
        if ready.page_count() < 2 {
            return;
        }
        let page = PageNo::from_index(1);
        let doc_gen = ready.open_gen;
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::PageData {
                page,
                doc_gen,
                result: Err("boom".into()),
            },
        );
        let Session::Ready(ready) = &session else {
            panic!("expected Ready");
        };
        assert!(ready.page_data_failed.contains(&page.index()));
        assert!(
            ready.next_page_data_target().is_none() || ready.next_page_data_target() != Some(page)
        );
    }

    #[test]
    fn search_overlapping_ranges_keep_glyph_quads() {
        let layer = TextLayer {
            page: PageNo::first(),
            plain: "aa".into(),
            glyphs: vec![
                Glyph {
                    cluster: "a".into(),
                    quad: Quad::from_rect(0.0, 0.0, 1.0, 1.0),
                },
                Glyph {
                    cluster: "a".into(),
                    quad: Quad::from_rect(1.0, 0.0, 2.0, 1.0),
                },
            ],
        };
        let hits = find_hits("aa", &layer);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].quad.x1 > 1.0);
    }

    #[test]
    fn thumb_cache_drops_old_scale() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let page = ready.visible;
        let s1 = Scale::from_factor(1.0);
        let s2 = Scale::from_factor(1.5);
        let mut thumbs = ThumbCache::default();
        thumbs.insert(page, s1, fake_surface(page, s1));
        thumbs.insert(page, s2, fake_surface(page, s2));
        assert!(thumbs.get(page, s1).is_none());
        assert!(thumbs.get(page, s2).is_some());
    }

    #[test]
    fn prefetch_budget_blocks_oversized_neighbor() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        if ready.page_count() < 2 {
            return;
        }
        ready.visible = PageNo::first();
        let next = PageNo::from_index(1);
        ready.pages.media[1] = Some(MediaBox {
            width: 2000.0,
            height: 2000.0,
        });
        let huge = Scale::from_factor(100.0);
        assert!(!ready.prefetch_fits_budget(next, huge));
    }

    fn sample_ready() -> Option<Ready> {
        let path = sample_pdf();
        let bytes = std::fs::read(&path).ok()?;
        Document::from_bytes(OpenSource::Path(path), Arc::<[u8]>::from(bytes)).ok()
    }

    #[test]
    fn set_theme_updates_state_and_persists() {
        let prefs =
            std::env::temp_dir().join(format!("tsuro-prefs-unit-{}-theme", std::process::id()));
        crate::prefs::with_prefs_path(prefs.clone(), || {
            let _ = std::fs::remove_file(&prefs);
            let mut session = Session::empty();
            assert_eq!(session.theme(), Theme::Dark);
            apply(&mut session, Message::SetTheme(Theme::Light));
            assert_eq!(session.theme(), Theme::Light);
            assert_eq!(crate::prefs::read_theme(), Theme::Light);
            assert_eq!(Session::empty().theme(), Theme::Light);
            apply(&mut session, Message::SetTheme(Theme::Dark));
            assert_eq!(crate::prefs::read_theme(), Theme::Dark);
        });
        let _ = std::fs::remove_file(&prefs);
    }

    #[test]
    fn theme_survives_open_overflow_and_close() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Empty(EmptyState {
            theme: Theme::Light,
            ..EmptyState::default()
        });
        let _ = session.begin_open(OpenSource::Path(sample_pdf()));
        let gen = match &session {
            Session::Loading { gen, .. } => *gen,
            other => panic!("expected Loading, got {other:?}"),
        };
        assert_eq!(session.theme(), Theme::Light);
        let _ = session.update(Message::Opened {
            gen,
            result: Ok(ready),
        });
        let Session::Ready(r) = &session else {
            panic!("expected Ready");
        };
        assert_eq!(r.theme, Theme::Light);
        assert!(!r.overflow_open);
        apply(&mut session, Message::ToggleOverflow);
        let Session::Ready(r) = &session else {
            panic!("expected Ready");
        };
        assert!(r.overflow_open);
        apply(&mut session, Message::SetZoom(Zoom::Width));
        let Session::Ready(r) = &session else {
            panic!("expected Ready");
        };
        assert!(!r.overflow_open);
        apply(&mut session, Message::Close);
        match &session {
            Session::Empty(empty) => assert_eq!(empty.theme, Theme::Light),
            other => panic!("expected Empty, got {other:?}"),
        }
        apply(&mut session, Message::ToggleOverflow);
        assert!(matches!(session, Session::Empty(_)));
    }

    #[test]
    fn window_scale_sets_dpr_and_ignores_garbage() {
        let mut session = Session::Empty(EmptyState {
            render_scale: 1.0,
            ..EmptyState::default()
        });
        apply(&mut session, Message::WindowScale(2.0));
        assert_eq!(session.render_scale(), 2.0);
        apply(&mut session, Message::WindowScale(f32::NAN));
        apply(&mut session, Message::WindowScale(0.0));
        assert_eq!(session.render_scale(), 2.0);
        apply(&mut session, Message::WindowScale(2.0));
        assert_eq!(session.render_scale(), 2.0);
    }

    #[test]
    fn page_scale_multiplies_css_zoom_by_dpr() {
        let Some(mut ready) = sample_ready() else {
            return;
        };
        // `Scale` quantiza em 1/1000: compara fator com tolerância, senão o
        // arredondado × 2 diverge do dobro arredondado em 1 unidade.
        let css = ready.sheet_css(ready.visible);
        assert!((ready.page_scale(ready.visible).factor() - css).abs() < 0.002);
        ready.render_scale = 2.0;
        assert!((ready.page_scale(ready.visible).factor() - css * 2.0).abs() < 0.002);
        ready.render_scale = 0.0;
        assert!((ready.page_scale(ready.visible).factor() - css).abs() < 0.002);
    }

    #[test]
    fn render_scale_survives_open_and_close() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Empty(EmptyState {
            render_scale: 2.0,
            ..EmptyState::default()
        });
        let _ = session.begin_open(OpenSource::Path(sample_pdf()));
        assert_eq!(session.render_scale(), 2.0);
        let gen = match &session {
            Session::Loading { gen, .. } => *gen,
            other => panic!("expected Loading, got {other:?}"),
        };
        let _ = session.update(Message::Opened {
            gen,
            result: Ok(ready),
        });
        match &session {
            Session::Ready(r) => assert_eq!(r.render_scale, 2.0),
            other => panic!("expected Ready, got {other:?}"),
        }
        apply(&mut session, Message::Close);
        match &session {
            Session::Empty(empty) => assert_eq!(empty.render_scale, 2.0),
            other => panic!("expected Empty, got {other:?}"),
        }
    }

    fn open_dialog() -> Option<Session> {
        let Some(mut ready) = sample_ready() else {
            return None;
        };
        ready.overflow_open = true;
        ready.print_status = Some("status antigo".into());
        let mut session = Session::Ready(Tabs::single(ready));
        apply(&mut session, Message::OpenPrintDialog);
        apply(
            &mut session,
            Message::PrintersLoaded(vec![
                PrinterInfo {
                    name: "Laser".into(),
                    is_default: false,
                },
                PrinterInfo {
                    name: "Jato".into(),
                    is_default: true,
                },
            ]),
        );
        Some(session)
    }

    fn dialog(session: &Session) -> &PrintDialog {
        let Session::Ready(ready) = session else {
            panic!("expected Ready, got {session:?}");
        };
        ready.print_dialog.as_ref().expect("dialog open")
    }

    #[test]
    fn print_dialog_on_empty_is_noop() {
        let mut session = Session::empty();
        apply(&mut session, Message::OpenPrintDialog);
        assert!(matches!(session, Session::Empty(_)));
        apply(&mut session, Message::PrintSubmit);
        assert!(matches!(session, Session::Empty(_)));
        apply(&mut session, Message::ClosePrintDialog);
        assert!(matches!(session, Session::Empty(_)));
    }

    #[test]
    fn open_dialog_resets_and_preselects_default_printer() {
        let Some(session) = open_dialog() else {
            return;
        };
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        assert!(!ready.overflow_open);
        assert!(ready.print_status.is_none());
        let dialog = dialog(&session);
        assert!(!dialog.printers_loading);
        assert_eq!(dialog.selected, Some(1));
        assert_eq!(dialog.copies, 1);
        assert_eq!(dialog.range_mode, RangeMode::All);
        assert!(!dialog.busy);
    }

    #[test]
    fn printers_loaded_without_default_selects_first() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        apply(&mut session, Message::OpenPrintDialog);
        apply(
            &mut session,
            Message::PrintersLoaded(vec![PrinterInfo {
                name: "Só".into(),
                is_default: false,
            }]),
        );
        assert_eq!(dialog(&session).selected, Some(0));
        apply(&mut session, Message::PrintersLoaded(vec![]));
        let d = dialog(&session);
        assert!(d.selected.is_none());
        assert!(!d.printers_loading);
    }

    #[test]
    fn dialog_controls_update_state() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintSelectPrinter(0));
        assert_eq!(dialog(&session).selected, Some(0));
        apply(&mut session, Message::PrintSelectPrinter(9));
        assert_eq!(dialog(&session).selected, Some(0));
        apply(&mut session, Message::PrintSetRangeMode(RangeMode::Custom));
        apply(&mut session, Message::PrintSetFromInput("2".into()));
        apply(&mut session, Message::PrintSetToInput("1".into()));
        let d = dialog(&session);
        assert_eq!(d.range_mode, RangeMode::Custom);
        apply(&mut session, Message::PrintCopiesPlus);
        apply(&mut session, Message::PrintCopiesPlus);
        apply(&mut session, Message::PrintCopiesMinus);
        assert_eq!(dialog(&session).copies, 2);
        apply(
            &mut session,
            Message::PrintSetOrientation(crate::print::PrintOrientation::Landscape),
        );
        assert_eq!(
            dialog(&session).orientation,
            crate::print::PrintOrientation::Landscape
        );
        apply(&mut session, Message::PrintPreviewNext);
        apply(&mut session, Message::PrintPreviewPrev);
        apply(&mut session, Message::ClosePrintDialog);
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        assert!(ready.print_dialog.is_none());
    }

    #[test]
    fn copies_clamp_between_1_and_max() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintCopiesMinus);
        assert_eq!(dialog(&session).copies, 1);
        for _ in 0..200 {
            apply(&mut session, Message::PrintCopiesPlus);
        }
        assert_eq!(dialog(&session).copies, crate::print::MAX_COPIES);
    }

    #[test]
    fn submit_with_invalid_range_reports_error_and_stays_open() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintSetRangeMode(RangeMode::Custom));
        apply(&mut session, Message::PrintSetFromInput("2".into()));
        apply(&mut session, Message::PrintSetToInput("1".into()));
        apply(&mut session, Message::PrintSubmit);
        let d = dialog(&session);
        assert!(!d.busy);
        assert_eq!(d.error.as_deref(), Some("«De» maior que «Até»"));
    }

    #[test]
    fn submit_without_printer_reports_error() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        apply(&mut session, Message::OpenPrintDialog);
        apply(&mut session, Message::PrintersLoaded(vec![]));
        apply(&mut session, Message::PrintSubmit);
        let d = dialog(&session);
        assert!(!d.busy);
        assert_eq!(d.error.as_deref(), Some("nenhuma impressora selecionada"));
    }

    #[test]
    fn submit_marks_busy_and_second_submit_is_ignored() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintSubmit);
        assert!(dialog(&session).busy);
        apply(&mut session, Message::PrintSetRangeMode(RangeMode::Current));
        assert_eq!(dialog(&session).range_mode, RangeMode::All);
        assert!(dialog(&session).busy);
    }

    #[test]
    fn submitted_ok_closes_dialog_and_sets_status() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintSubmit);
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        let doc_gen = ready.open_gen;
        apply(
            &mut session,
            Message::PrintSubmitted {
                doc_gen,
                printer: "Jato".into(),
                result: Ok(7),
            },
        );
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        assert!(ready.print_dialog.is_none());
        assert_eq!(
            ready.print_status.as_deref(),
            Some("Enviado para Jato (job 7)")
        );
    }

    #[test]
    fn submitted_err_keeps_dialog_open_with_error() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintSubmit);
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        let doc_gen = ready.open_gen;
        apply(
            &mut session,
            Message::PrintSubmitted {
                doc_gen,
                printer: "Jato".into(),
                result: Err("spool cheio".into()),
            },
        );
        let d = dialog(&session);
        assert!(!d.busy);
        assert_eq!(d.error.as_deref(), Some("spool cheio"));
    }

    #[test]
    fn submitted_with_stale_doc_gen_is_ignored() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintSubmit);
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        let stale = ready.open_gen.wrapping_add(1);
        apply(
            &mut session,
            Message::PrintSubmitted {
                doc_gen: stale,
                printer: "Jato".into(),
                result: Err("tarde demais".into()),
            },
        );
        let d = dialog(&session);
        assert!(d.busy);
        assert!(d.error.is_none());
    }

    #[test]
    fn open_pdf_hatch_keeps_dialog_open() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintOpenPdf);
        assert!(dialog(&session).busy);
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        let doc_gen = ready.open_gen;
        apply(
            &mut session,
            Message::PrintPdfOpened {
                doc_gen,
                result: Ok("/tmp/x.pdf".into()),
            },
        );
        let d = dialog(&session);
        assert!(!d.busy);
        assert!(d.error.is_none());
    }

    #[test]
    fn dialog_preview_pages_follow_range() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        let count = ready.page_count();
        let current = ready.visible;
        let pages = dialog(&session).preview_pages(count, current);
        assert_eq!(pages.len() as u32, count);
        apply(&mut session, Message::PrintSetRangeMode(RangeMode::Current));
        let pages = dialog(&session).preview_pages(count, current);
        assert_eq!(pages, vec![current]);
        apply(&mut session, Message::PrintSetRangeMode(RangeMode::Custom));
        apply(&mut session, Message::PrintSetFromInput("999".into()));
        let pages = dialog(&session).preview_pages(count, current);
        assert_eq!(pages, vec![current]);
    }

    #[test]
    fn escape_maps_to_close_dialog() {
        use iced::event::Status;
        use iced::keyboard::{key::Named, Key, Modifiers};
        assert!(matches!(
            keyboard_message(
                Key::Named(Named::Escape),
                Modifiers::default(),
                Status::Ignored
            ),
            Some(Message::ClosePrintDialog)
        ));
    }

    #[test]
    fn close_while_busy_is_ignored() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintSubmit);
        apply(&mut session, Message::ClosePrintDialog);
        assert!(dialog(&session).busy);
    }

    #[test]
    fn nop_keeps_dialog_open() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::PrintNop);
        assert!(!dialog(&session).busy);
    }

    #[test]
    fn nav_pages_preview_with_dialog_open() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        apply(&mut session, Message::Nav(NavCmd::Next));
        assert_eq!(dialog(&session).preview, 1);
        let Session::Ready(ready) = &session else {
            panic!("expected Ready");
        };
        assert_eq!(ready.visible, PageNo::first());
        apply(&mut session, Message::Nav(NavCmd::Previous));
        assert_eq!(dialog(&session).preview, 0);
        apply(&mut session, Message::Nav(NavCmd::Last));
        assert_eq!(dialog(&session).preview, 1);
        apply(&mut session, Message::Nav(NavCmd::First));
        assert_eq!(dialog(&session).preview, 0);
        apply(
            &mut session,
            Message::Nav(NavCmd::GoTo(PageNo::from_index(1))),
        );
        assert_eq!(dialog(&session).preview, 1);
        apply(&mut session, Message::ClosePrintDialog);
        apply(&mut session, Message::Nav(NavCmd::Next));
        let Session::Ready(ready) = &session else {
            panic!("expected Ready");
        };
        assert_eq!(ready.visible, PageNo::from_index(1));
    }

    #[test]
    fn rendered_inserts_preview_thumb_with_panel_closed() {
        if sample_ready().is_none_or(|ready| ready.page_count() < 2) {
            return;
        }
        let Some(mut session) = open_dialog() else {
            return;
        };
        // Alvo fora da janela inicial: página 2 em modo De–Até.
        apply(&mut session, Message::PrintSetRangeMode(RangeMode::Custom));
        apply(&mut session, Message::PrintSetFromInput("2".into()));
        apply(&mut session, Message::PrintSetToInput("2".into()));
        let Session::Ready(ready) = &mut session else {
            panic!("expected Ready");
        };
        assert!(!ready.pages_open);
        let page = PageNo::from_index(1);
        // Media além da primeira é lazy: semeia como os demais testes de thumb.
        let media = ready.loaded_media(PageNo::first()).expect("media");
        ready.pages.media[1] = Some(media);
        let thumb_scale = ready.thumb_scale_for(media);
        assert_ne!(ready.page_scale(page), thumb_scale);
        let (doc_gen, render_gen) = (ready.open_gen, ready.render_gen);
        apply(
            &mut session,
            Message::Rendered {
                page,
                scale: thumb_scale,
                rotation: 0,
                doc_gen,
                render_gen,
                surface: Some(fake_surface(page, thumb_scale)),
            },
        );
        let Session::Ready(ready) = &session else {
            panic!("expected Ready");
        };
        assert!(ready.thumb_surface(page).is_some());
    }

    #[test]
    fn evict_keeps_preview_thumb_and_drops_others() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        let Session::Ready(ready) = &mut session else {
            panic!("expected Ready");
        };
        assert!(!ready.pages_open);
        let current = PageNo::from_index(0);
        let other = PageNo::from_index(1);
        let media0 = ready.loaded_media(current).expect("media");
        ready.pages.media[1] = Some(media0);
        for page in [current, other] {
            let media = ready.loaded_media(page).expect("media");
            let scale = ready.thumb_scale_for(media);
            ready.thumbs.insert(page, scale, fake_surface(page, scale));
        }
        let (doc_gen, render_gen) = (ready.open_gen, ready.render_gen);
        apply(
            &mut session,
            Message::Rendered {
                page: current,
                scale: Scale::from_factor(9.0),
                rotation: 0,
                doc_gen,
                render_gen,
                surface: None,
            },
        );
        let Session::Ready(ready) = &session else {
            panic!("expected Ready");
        };
        assert!(ready.thumb_surface(current).is_some());
        assert!(ready.thumb_surface(other).is_none());
    }

    #[test]
    fn thumb_render_is_scheduled_for_preview_target() {
        let Some(mut session) = open_dialog() else {
            return;
        };
        let Session::Ready(ready) = &mut session else {
            panic!("expected Ready");
        };
        // Simula documento já lido: superfície da visível existe.
        let visible = ready.visible;
        let scale = ready.page_scale(visible);
        ready
            .surfaces
            .insert(visible, scale, 0, fake_surface(visible, scale));
        let target = PageNo::from_index(0);
        let media = ready.loaded_media(target).expect("media");
        let key = render_key(target, ready.thumb_scale_for(media), 0);
        // `open_dialog` já agendou o render da visível; simula a worker livre.
        ready.render_inflight.clear();
        let _ = ready.request_thumb_render();
        assert!(ready.render_inflight.contains(&key));
    }

    #[test]
    fn suggested_marked_name_replaces_only_the_last_extension() {
        use std::path::Path;
        assert_eq!(
            suggested_marked_name(Path::new("/tmp/contrato.pdf")),
            "contrato (marcado).pdf"
        );
        assert_eq!(
            suggested_marked_name(Path::new("/tmp/contrato")),
            "contrato (marcado).pdf"
        );
        assert_eq!(
            suggested_marked_name(Path::new("/tmp/ata.v1.tar.gz")),
            "ata.v1.tar (marcado).pdf"
        );
    }

    /// Assinatura de mentira: o fluxo só olha a lista estar vazia ou não.
    fn one_signature() -> tsuro_sign::SignatureInfo {
        tsuro_sign::SignatureInfo {
            field_name: None,
            signer_name: None,
            reason: None,
            location: None,
            contact_info: None,
            signing_time: None,
            filter: None,
            sub_filter: None,
            byte_range: None,
            covers_whole_document: true,
            status: tsuro_sign::SignatureStatus::IntactButUntrusted,
            status_detail: String::new(),
            certificate: None,
            digest_algorithm: None,
            signature_algorithm: None,
        }
    }

    #[test]
    fn sign_warning_only_for_signed_documents() {
        let analysis = |signatures| PdfAnalysis {
            page_count_hint: Some(1),
            has_acro_form: false,
            signatures,
        };
        assert!(!needs_sign_warning(&analysis(Vec::new())));
        assert!(needs_sign_warning(&analysis(vec![one_signature()])));
    }

    #[test]
    fn same_file_detects_original_through_path_aliases() {
        let dir = std::env::temp_dir().join(format!(
            "tsuro-save-same-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("doc.pdf");
        std::fs::write(&src, b"%PDF-1.4").unwrap();
        assert!(is_same_file(&src, &src));
        assert!(is_same_file(&dir.join(".").join("doc.pdf"), &src));
        // Destino novo (ainda inexistente) nunca é o original.
        assert!(!is_same_file(&dir.join("doc (marcado).pdf"), &src));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_without_annotations_sets_status_and_opens_nothing() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        apply(&mut session, Message::SaveCopyRequested);
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        assert_eq!(
            ready.save_status.as_deref(),
            Some("Nada para salvar — marque o texto primeiro.")
        );
        assert!(!ready.save_warning);
    }

    #[test]
    fn save_done_ok_records_copy_in_recents() {
        isolated(|| {
            let Some(ready) = sample_ready() else {
                return;
            };
            let dest = std::env::temp_dir().join("contrato (marcado).pdf");
            let doc_gen = ready.open_gen;
            let mut session = Session::Ready(Tabs::single(ready));
            apply(
                &mut session,
                Message::SaveCopyDone {
                    doc_gen,
                    path: dest.clone(),
                    result: Ok(()),
                },
            );
            let Session::Ready(ready) = &session else {
                panic!("expected Ready, got {session:?}");
            };
            assert_eq!(
                ready.save_status.as_deref(),
                Some("Cópia salva em contrato (marcado).pdf")
            );
            assert_eq!(ready.recents.first(), Some(&dest));
            assert_eq!(read_recents().first(), Some(&dest));
        });
    }

    #[test]
    fn save_done_err_sets_failure_status() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let doc_gen = ready.open_gen;
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::SaveCopyDone {
                doc_gen,
                path: std::env::temp_dir().join("x.pdf"),
                result: Err("disco cheio".into()),
            },
        );
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        assert_eq!(
            ready.save_status.as_deref(),
            Some("Falha ao salvar: disco cheio")
        );
    }

    #[test]
    fn save_done_with_stale_doc_gen_is_ignored() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let stale = ready.open_gen.wrapping_add(1);
        let mut session = Session::Ready(Tabs::single(ready));
        apply(
            &mut session,
            Message::SaveCopyDone {
                doc_gen: stale,
                path: std::env::temp_dir().join("x.pdf"),
                result: Ok(()),
            },
        );
        let Session::Ready(ready) = &session else {
            panic!("expected Ready, got {session:?}");
        };
        assert!(ready.save_status.is_none());
    }
    #[test]
    fn zoom_out_to_min_renders_and_views_every_step() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        let mut current = 1.0f32;
        for step in 0..40 {
            current /= 1.1;
            apply(
                &mut session,
                Message::SetZoom(Zoom::Manual(ZoomFactor::new(current))),
            );
            let (page, scale, doc_gen, render_gen, surface) = {
                let Session::Ready(ready) = &session else {
                    panic!("sessao saiu de Ready no passo {step}");
                };
                let Zoom::Manual(z) = ready.zoom else {
                    panic!("zoom trocado sozinho no passo {step}");
                };
                assert!(z.get() >= 0.25, "clamp furou: {}", z.get());
                let page = ready.visible;
                let scale = ready.page_scale(page);
                let surface = crate::page::PageEngine::render(&ready.engine, page, scale, 0)
                    .expect("render real falhou");
                assert!(surface.bitmap.width > 0 && surface.bitmap.height > 0);
                assert_eq!(
                    surface.bitmap.rgba.len(),
                    surface.bitmap.width as usize * surface.bitmap.height as usize * 4
                );
                (page, scale, ready.open_gen, ready.render_gen, surface)
            };
            apply(
                &mut session,
                Message::Rendered {
                    page,
                    scale,
                    rotation: 0,
                    doc_gen,
                    render_gen,
                    surface: Some(surface),
                },
            );
            let _ = session.view();
        }
    }
    #[test]
    fn stale_and_failed_renders_recover_on_next_zoom() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        let gen0 = match &session {
            Session::Ready(r) => r.render_gen,
            _ => unreachable!(),
        };
        // Rajada de zoom-out sem alimentar completions (inflight acumula).
        let mut current = 1.0f32;
        for _ in 0..10 {
            current /= 1.1;
            apply(
                &mut session,
                Message::SetZoom(Zoom::Manual(ZoomFactor::new(current))),
            );
            let _ = session.view();
        }
        let (page, doc_gen, render_gen) = match &session {
            Session::Ready(r) => (r.visible, r.open_gen, r.render_gen),
            _ => panic!("saiu de Ready na rajada"),
        };
        assert!(render_gen > gen0);
        // Completion velha (gen antiga, escala errada): tem que ignorar.
        apply(
            &mut session,
            Message::Rendered {
                page,
                scale: Scale::from_factor(9.0),
                rotation: 0,
                doc_gen,
                render_gen: gen0,
                surface: Some(fake_surface(page, Scale::from_factor(9.0))),
            },
        );
        let _ = session.view();
        // Falha na gen atual: marca failed, nao quebra.
        let scale_now = match &session {
            Session::Ready(r) => r.page_scale(page),
            _ => panic!("saiu de Ready"),
        };
        apply(
            &mut session,
            Message::Rendered {
                page,
                scale: scale_now,
                rotation: 0,
                doc_gen,
                render_gen,
                surface: None,
            },
        );
        let _ = session.view();
        // Zoom novo (nova chave) + render real: recupera com superficie.
        apply(
            &mut session,
            Message::SetZoom(Zoom::Manual(ZoomFactor::new(0.5))),
        );
        let (scale2, dg2, rg2, surface) = match &session {
            Session::Ready(r) => {
                let s = r.page_scale(page);
                let surf = crate::page::PageEngine::render(&r.engine, page, s, 0)
                    .expect("render real falhou");
                (s, r.open_gen, r.render_gen, surf)
            }
            _ => panic!("saiu de Ready"),
        };
        apply(
            &mut session,
            Message::Rendered {
                page,
                scale: scale2,
                rotation: 0,
                doc_gen: dg2,
                render_gen: rg2,
                surface: Some(surface),
            },
        );
        let Session::Ready(r) = &session else {
            panic!("saiu de Ready na recuperacao");
        };
        assert!(
            r.surface(page, scale2).is_some(),
            "sem superficie apos recuperar"
        );
        let _ = session.view();
    }
    #[test]
    fn continuous_placeholders_do_not_panic_scrollable() {
        let Some(ready) = sample_ready() else {
            return;
        };
        let mut session = Session::Ready(Tabs::single(ready));
        let _ = session.view();
        apply(&mut session, Message::SetViewMode(ViewMode::Continuous));
        let _ = session.view();
    }
}
