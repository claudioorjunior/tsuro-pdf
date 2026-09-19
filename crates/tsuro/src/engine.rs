use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, LazyLock};

use pdfium_render::prelude::*;
use unicode_normalization::UnicodeNormalization;

use crate::page::{
    Bitmap, EngineError, Glyph, MediaBox, Outline, OutlineItem, PageEngine, PageNo, PageSurface,
    Quad, Scale, TextLayer,
};
use crate::session::{AnnotKind, Annotation};

const PDFIUM_MISSING: &str =
    "Não foi possível carregar a biblioteca Pdfium (.dylib/.dll). Coloque-a na pasta do aplicativo ou instale-a no sistema.";
const WORKER_GONE: &str = "motor PDF encerrado";
const DOC_GONE: &str = "documento fechado";
const RENDER_TOO_LARGE: &str = "página grande demais para renderizar nesta escala";
const RENDER_INVALID: &str = "dimensão de render inválida";
const ANNOT_PAGE_OUT_OF_RANGE: &str = "página da marcação fora do intervalo";
const ANNOT_MARK_FAILED: &str = "não foi possível gravar a marcação no PDF";
const ANNOT_SAVE_FAILED: &str = "não foi possível salvar a cópia marcada do PDF";
#[cfg(test)]
const ANNOT_READ_FAILED: &str = "não foi possível ler as anotações do PDF";

/// Lado (pt) do marcador de nota gravado no PDF. Mesma âncora do marcador da
/// UI: canto superior esquerdo do primeiro quad, crescendo para baixo.
const ANNOT_NOTE_SIDE: f32 = 12.0;

/// Lado máximo em px. A4 a 8× (teto de zoom da UI) fica em ~4760×6736.
const MAX_RENDER_SIDE: u32 = 16_384;
/// Teto de pixels RGBA (bytes = este valor × 4). 64M ≈ 256 MiB; A4 a 8× ≈ 32M.
const MAX_RENDER_PIXELS: u32 = 64_000_000;

// Todos os documentos abertos vivem numa única worker thread do processo, e
// cada operação é uma ida-e-volta leve pelo canal. Uma thread só (e um
// `Pdfium` só) não é economia de recurso: o `pdfium-render` embrulha a
// biblioteca num mutex global e o segura de `FPDF_InitLibrary` até
// `FPDF_DestroyLibrary`, então um segundo `Pdfium` vivo no mesmo processo
// trava para sempre. Uma aba por documento (issue #40) exige, portanto, um
// motor por processo servindo N documentos.
#[derive(Clone)]
pub struct PdfiumEngine {
    worker: Arc<Shared>,
    /// Documento dentro da worker (o motor serve vários ao mesmo tempo).
    doc_id: u64,
    page_count: u32,
}

struct Shared {
    requests: mpsc::Sender<Request>,
}

enum Request {
    /// Carrega os bytes e devolve o `doc_id` com que a worker vai servir o
    /// documento (ids são da worker: quem abre não escolhe).
    Open {
        bytes: Vec<u8>,
        reply: mpsc::Sender<Result<(u64, u32), EngineError>>,
    },
    /// Esquece o documento (aba fechada). Sem resposta: é limpeza.
    Close { doc_id: u64 },
    PageData {
        doc_id: u64,
        page: PageNo,
        reply: mpsc::Sender<Result<(MediaBox, TextLayer), EngineError>>,
    },
    Render {
        doc_id: u64,
        page: PageNo,
        scale: Scale,
        /// Quartos de volta horários da vista (0..=3); impressão usa 0.
        rotation: u8,
        reply: mpsc::Sender<Result<PageSurface, EngineError>>,
    },
    /// Lê o outline (bookmarks) do documento. Read-only: não altera o
    /// comportamento interno do Pdfium, apenas percorre a árvore existente.
    Outline {
        doc_id: u64,
        reply: mpsc::Sender<Result<Option<Outline>, EngineError>>,
    },
    /// Aplica as marcações da sessão ao documento vivo e devolve uma cópia
    /// em bytes (`FPDF_SaveAsCopy`). O documento em memória passa a conter as
    /// anotações; o mesmo motor continua servindo as páginas marcadas.
    SaveCopy {
        doc_id: u64,
        annotations: Vec<Annotation>,
        reply: mpsc::Sender<Result<Vec<u8>, EngineError>>,
    },
    #[cfg(test)]
    /// Lê as anotações de uma página do documento vivo, como dados puros
    /// (`StoredAnnotation`). Existe para o teste de `save_copy` ler de volta
    /// pela worker: um segundo `Pdfium` no processo travaria no mutex global.
    Annotations {
        doc_id: u64,
        page: PageNo,
        reply: mpsc::Sender<Result<Vec<StoredAnnotation>, EngineError>>,
    },
}

/// Dono do único `Pdfium` do processo: num `static`, o empréstimo que os
/// `PdfDocument` fazem dele é `'static` — então os documentos podem morar num
/// mapa em vez de morrer no fim de cada requisição.
struct Library(Pdfium);

// O `LazyLock` exige `Send + Sync` do valor, e o handle do pdfium-render não
// é nenhum dos dois por tipo (guarda um `RefCell` com o token do mutex da
// biblioteca). Aqui há um dono só e uma thread só: quem inicializa o Pdfium
// (`FPDF_InitLibrary`, chamado de dentro de `serve`) e quem faz as chamadas é
// a worker, que nunca entrega o handle a ninguém — os documentos vivem todos
// dentro dela. Não existe acesso concorrente; os markers só registram esse
// contrato, sem estender lifetime nenhum.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

/// Worker do processo: nasce no primeiro `open` e vive até o processo acabar
/// (uma `Pdfium` por processo — ver o comentário de `PdfiumEngine`).
static WORKER: LazyLock<Arc<Shared>> = LazyLock::new(|| {
    let (requests, incoming) = mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name("tsuro-pdfium".into())
        .spawn(move || serve(incoming))
        .expect("não foi possível iniciar a thread do motor PDF");
    Arc::new(Shared { requests })
});

/// A biblioteca do processo. O erro do bind é cacheado junto: sem Pdfium
/// nenhuma aba abre, e é isso que cada `open` responde.
static LIBRARY: LazyLock<Result<Library, String>> = LazyLock::new(|| {
    PdfiumEngine::bind()
        .map(Library)
        .map_err(|err| err.to_string())
});

fn worker() -> Arc<Shared> {
    WORKER.clone()
}

/// Corpo da worker: a `Pdfium` do processo (static) e um `PdfDocument` por
/// aba, identificado pelo `doc_id` que a worker mesma distribui.
fn serve(incoming: mpsc::Receiver<Request>) {
    let mut documents: HashMap<u64, PdfDocument<'static>> = HashMap::new();
    let mut next_id = 1u64;
    for request in incoming {
        match request {
            Request::Open { bytes, reply } => {
                let pdfium = match LIBRARY.as_ref() {
                    Ok(library) => &library.0,
                    Err(err) => {
                        let _ = reply.send(Err(EngineError(err.clone())));
                        continue;
                    }
                };
                match pdfium.load_pdf_from_byte_vec(bytes, None) {
                    Ok(document) => {
                        let doc_id = next_id;
                        next_id += 1;
                        let count = u32::from(document.pages().len());
                        documents.insert(doc_id, document);
                        let _ = reply.send(Ok((doc_id, count)));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(EngineError(format!(
                            "não foi possível abrir o PDF: {e}"
                        ))));
                    }
                }
            }
            Request::Close { doc_id } => {
                documents.remove(&doc_id);
            }
            Request::PageData {
                doc_id,
                page,
                reply,
            } => {
                let _ = reply.send(with_doc(&documents, doc_id, |doc| {
                    page_data_from_doc(doc, page)
                }));
            }
            Request::Render {
                doc_id,
                page,
                scale,
                rotation,
                reply,
            } => {
                let _ = reply.send(with_doc(&documents, doc_id, |doc| {
                    render_from_doc(doc, page, scale, rotation)
                }));
            }
            Request::Outline { doc_id, reply } => {
                let _ = reply.send(with_doc(&documents, doc_id, outline_from_doc));
            }
            Request::SaveCopy {
                doc_id,
                annotations,
                reply,
            } => {
                let _ = reply.send(with_doc(&documents, doc_id, |doc| {
                    save_copy_from_doc(doc, &annotations)
                }));
            }
            #[cfg(test)]
            Request::Annotations {
                doc_id,
                page,
                reply,
            } => {
                let _ = reply.send(with_doc(&documents, doc_id, |doc| {
                    annotations_from_doc(doc, page)
                }));
            }
        }
    }
}

/// Roda `f` no documento `doc_id`; sem ele (aba fechada no meio do caminho) o
/// pedido falha, nunca entra em pânico.
fn with_doc<T>(
    documents: &HashMap<u64, PdfDocument<'static>>,
    doc_id: u64,
    f: impl FnOnce(&PdfDocument<'_>) -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    match documents.get(&doc_id) {
        Some(document) => f(document),
        None => Err(EngineError(DOC_GONE.into())),
    }
}

impl PdfiumEngine {
    fn bind() -> Result<Pdfium, EngineError> {
        // Bundle .app, depois a lib ao lado do binário. Nunca o cwd:
        // um PDF numa pasta com libpdfium plantada não deve ser carregado.
        for path in pdfium_library_candidates() {
            if let Ok(bindings) = Pdfium::bind_to_library(&path) {
                return Ok(Pdfium::new(bindings));
            }
        }
        Pdfium::bind_to_system_library()
            .map(Pdfium::new)
            .map_err(|_| EngineError(PDFIUM_MISSING.into()))
    }

    fn call<T>(
        &self,
        make: impl FnOnce(mpsc::Sender<Result<T, EngineError>>) -> Request,
    ) -> Result<T, EngineError> {
        let (tx, rx) = mpsc::channel();
        self.worker
            .requests
            .send(make(tx))
            .map_err(|_| EngineError(WORKER_GONE.into()))?;
        rx.recv().map_err(|_| EngineError(WORKER_GONE.into()))?
    }

    // Media + texto numa única ida à worker (antes eram dois reloads).
    pub fn page_data(&self, page: PageNo) -> Result<(MediaBox, TextLayer), EngineError> {
        self.call(|reply| Request::PageData {
            doc_id: self.doc_id,
            page,
            reply,
        })
    }

    /// Lê o outline (bookmarks) do documento. Read-only sobre `PdfDocument`;
    /// não altera o estado interno do Pdfium.
    pub fn outline(&self) -> Result<Option<Outline>, EngineError> {
        self.call(|reply| Request::Outline {
            doc_id: self.doc_id,
            reply,
        })
    }

    #[cfg(test)]
    /// Lê as anotações de uma página, como dados puros. Read-only sobre
    /// `PdfDocument`; serve ao teste de `save_copy` (ver `Request::Annotations`).
    pub fn annotations(&self, page: PageNo) -> Result<Vec<StoredAnnotation>, EngineError> {
        self.call(|reply| Request::Annotations {
            doc_id: self.doc_id,
            page,
            reply,
        })
    }

    /// Grava uma cópia do documento aberto com as marcações da sessão
    /// aplicadas, devolvendo os bytes do PDF resultante.
    ///
    /// As anotações entram no documento vivo da worker (sem reload), então o
    /// próprio motor passa a servir a cópia marcada. O `PdfDocument` continua
    /// compartilhado por referência: só a `PdfPage` emprestada é mutável para
    /// criar anotações, e `save_to_bytes` recebe `&self`.
    pub fn save_copy(&self, annotations: &[Annotation]) -> Result<Vec<u8>, EngineError> {
        self.call(|reply| Request::SaveCopy {
            doc_id: self.doc_id,
            annotations: annotations.to_vec(),
            reply,
        })
    }

    /// Solta o documento na worker (aba fechada, issue #40): o parse sai da
    /// memória. Depois disto qualquer pedido deste motor falha — os clones
    /// (`Ready` é `Clone`) apontam para o mesmo `doc_id`.
    pub fn close(&self) {
        let _ = self.worker.requests.send(Request::Close {
            doc_id: self.doc_id,
        });
    }
}

impl PageEngine for PdfiumEngine {
    /// Carrega o documento na worker do processo e devolve o motor desta aba.
    /// Cada documento tem o seu `doc_id`; várias abas convivem no mesmo
    /// `Pdfium` (um por processo — ver o comentário de `PdfiumEngine`).
    fn open(bytes: Arc<[u8]>) -> Result<Self, EngineError> {
        let worker = worker();
        let (reply, answer) = mpsc::channel();
        worker
            .requests
            .send(Request::Open {
                bytes: bytes.to_vec(),
                reply,
            })
            .map_err(|_| EngineError(WORKER_GONE.into()))?;
        let (doc_id, page_count) = answer
            .recv()
            .map_err(|_| EngineError(WORKER_GONE.into()))??;
        Ok(Self {
            worker,
            doc_id,
            page_count,
        })
    }

    fn page_count(&self) -> u32 {
        self.page_count
    }

    fn media(&self, page: PageNo) -> Result<MediaBox, EngineError> {
        self.page_data(page).map(|(media, _)| media)
    }

    fn render(&self, page: PageNo, scale: Scale, rotation: u8) -> Result<PageSurface, EngineError> {
        self.call(|reply| Request::Render {
            doc_id: self.doc_id,
            page,
            scale,
            rotation,
            reply,
        })
    }

    fn text_layer(&self, page: PageNo) -> Result<TextLayer, EngineError> {
        self.page_data(page).map(|(_, text)| text)
    }
}

fn page_data_from_doc(
    document: &PdfDocument<'_>,
    page: PageNo,
) -> Result<(MediaBox, TextLayer), EngineError> {
    let pdf_page = document
        .pages()
        .get(page_index(page)?)
        .map_err(|e| EngineError(e.to_string()))?;
    let media = MediaBox {
        width: pdf_page.width().value,
        height: pdf_page.height().value,
    };
    let text = text_layer_from_page(&pdf_page, page)?;
    Ok((media, text))
}

/// Percorre recursivamente a árvore de bookmarks do Pdfium e produz
/// `Option<Outline>`: `None` quando não há bookmark raiz, `Some` quando há.
/// Read-only sobre o `PdfDocument` — não altera estado interno do Pdfium.
fn outline_from_doc(document: &PdfDocument<'_>) -> Result<Option<Outline>, EngineError> {
    let total = document.pages().len() as u32;
    // `root()` é o primeiro bookmark de topo (não um contêiner): o nível
    // superior é ele mais `iter_siblings()` (que pula o próprio nó).
    let Some(first) = document.bookmarks().root() else {
        return Ok(None);
    };
    Ok(Some(Outline {
        items: std::iter::once(first.clone())
            .chain(first.iter_siblings())
            .map(|child| outline_node(&child, total))
            .collect(),
    }))
}

fn outline_node(bookmark: &PdfBookmark<'_>, total: u32) -> OutlineItem {
    let page = bookmark
        .destination()
        .and_then(|dest| dest.page_index().ok())
        .filter(|&idx| (idx as u32) < total)
        .map(|idx| PageNo::from_index(idx as u32))
        .unwrap_or_else(PageNo::first);
    let title = bookmark
        .title()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(String::new);
    let children = bookmark
        .iter_direct_children()
        .map(|child| outline_node(&child, total))
        .collect();
    OutlineItem {
        title,
        page,
        children,
    }
}

#[derive(Debug, Clone, Copy)]
struct RenderTarget {
    width: i32,
    height: i32,
}

fn finite_positive(value: f32) -> Result<f32, EngineError> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(EngineError(RENDER_INVALID.into()))
    }
}

fn px_from_f32(value: f32) -> Result<u32, EngineError> {
    if !value.is_finite() {
        return Err(EngineError(RENDER_INVALID.into()));
    }
    let rounded = value.round();
    if !rounded.is_finite() {
        return Err(EngineError(RENDER_INVALID.into()));
    }
    if rounded < 0.0 {
        return Err(EngineError(RENDER_INVALID.into()));
    }
    if rounded < 1.0 {
        return Ok(1);
    }
    if rounded > MAX_RENDER_SIDE as f32 {
        return Err(EngineError(RENDER_TOO_LARGE.into()));
    }
    // Já limitado a [1, 16384]; f32 representa estes inteiros com exatidão.
    #[allow(clippy::cast_possible_truncation)]
    let px = rounded as u16;
    Ok(u32::from(px))
}

fn effective_scale(target_px: u32, page_pt: f32) -> Result<f32, EngineError> {
    let scale = target_px as f32 / page_pt;
    if scale.is_finite() && scale > 0.0 {
        Ok(scale)
    } else {
        Err(EngineError(RENDER_INVALID.into()))
    }
}

/// Largura e altura em px para o render, *antes* de chamar o Pdfium.
/// Recusa NaN/inf/≤0, lado acima do teto, bitmap RGBA que não cabe, ou
/// escala efetiva `px / pts` não-finita (página subnormal: 1×1 passaria no
/// teto, mas o pdfium-render faz `target / source` em f32 → inf).
fn render_target_px(page_w: f32, page_h: f32, factor: f32) -> Result<RenderTarget, EngineError> {
    let page_w = finite_positive(page_w)?;
    let page_h = finite_positive(page_h)?;
    let factor = finite_positive(factor)?;

    let width_f = page_w * factor;
    let height_f = page_h * factor;
    if !width_f.is_finite() || !height_f.is_finite() {
        return Err(EngineError(RENDER_INVALID.into()));
    }

    let width = px_from_f32(width_f)?;
    let height = px_from_f32(height_f)?;
    if width > MAX_RENDER_SIDE || height > MAX_RENDER_SIDE {
        return Err(EngineError(RENDER_TOO_LARGE.into()));
    }

    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| EngineError(RENDER_TOO_LARGE.into()))?;
    if pixels > MAX_RENDER_PIXELS {
        return Err(EngineError(RENDER_TOO_LARGE.into()));
    }

    let bytes = u64::from(pixels)
        .checked_mul(4)
        .ok_or_else(|| EngineError(RENDER_TOO_LARGE.into()))?;
    let _: usize = usize::try_from(bytes).map_err(|_| EngineError(RENDER_TOO_LARGE.into()))?;

    // pdfium-render `apply_to_page` faz `(target as f32) / source` e, se inf,
    // aloca i32::MAX². Recusar *antes* de pedir o bitmap.
    let _ = effective_scale(width, page_w)?;
    let _ = effective_scale(height, page_h)?;

    Ok(RenderTarget {
        width: i32::try_from(width).map_err(|_| EngineError(RENDER_TOO_LARGE.into()))?,
        height: i32::try_from(height).map_err(|_| EngineError(RENDER_TOO_LARGE.into()))?,
    })
}

fn render_from_doc(
    document: &PdfDocument<'_>,
    page: PageNo,
    scale: Scale,
    rotation: u8,
) -> Result<PageSurface, EngineError> {
    let pdf_page = document
        .pages()
        .get(page_index(page)?)
        .map_err(|e| EngineError(e.to_string()))?;
    // Vista girada 90°/270° troca largura ↔ altura antes do alvo em px.
    let swap = rotation & 1 == 1;
    let (page_w, page_h) = if swap {
        (pdf_page.height().value, pdf_page.width().value)
    } else {
        (pdf_page.width().value, pdf_page.height().value)
    };
    let target = render_target_px(page_w, page_h, scale.factor())?;
    // Tamanho fixo: o pdfium-render não divide por MediaBox (evita inf em
    // página subnormal mesmo se o helper falhar em silêncio).
    let config = PdfRenderConfig::new()
        .set_fixed_size(target.width, target.height)
        .rotate(rotation_for(rotation), false);
    let bitmap = pdf_page
        .render_with_config(&config)
        .map_err(|e| EngineError(e.to_string()))?;
    let width = bitmap.width() as u32;
    let height = bitmap.height() as u32;
    let rgba = bitmap.as_rgba_bytes();
    Ok(PageSurface {
        bitmap: Bitmap {
            width,
            height,
            rgba,
        },
        scale,
    })
}

fn page_index(page: PageNo) -> Result<u16, EngineError> {
    u16::try_from(page.index()).map_err(|_| EngineError("página fora do intervalo".into()))
}

/// Aplica as marcações da sessão ao documento e devolve a cópia em bytes.
///
/// Roda na worker thread, sobre o `PdfDocument` vivo: cada `Annotation` vira
/// uma anotação real do Pdfium (`/Highlight`, `/Underline`, `/Strikeout` ou
/// `/Text`), no mesmo user space do texto — os `Quad` da sessão vêm dos
/// `tight_bounds()` dos chars lidos do próprio Pdfium, então entram como
/// estão, sem flip de Y. Página fora do intervalo aborta antes de qualquer
/// alteração (falha total, nunca cópia parcial).
fn save_copy_from_doc(
    document: &PdfDocument<'_>,
    annotations: &[Annotation],
) -> Result<Vec<u8>, EngineError> {
    let page_count = document.pages().len();
    if annotations
        .iter()
        .any(|a| a.page.index() >= u32::from(page_count))
    {
        return Err(EngineError(ANNOT_PAGE_OUT_OF_RANGE.into()));
    }

    for index in 0..page_count {
        let page_no = PageNo::from_index(u32::from(index));
        let marks: Vec<&Annotation> = annotations.iter().filter(|a| a.page == page_no).collect();
        if marks.is_empty() {
            continue;
        }
        let mut pdf_page = document
            .pages()
            .get(index)
            .map_err(|e| EngineError(format!("{ANNOT_MARK_FAILED}: {e}")))?;
        {
            let target = pdf_page.annotations_mut();
            for mark in marks {
                match mark.kind {
                    AnnotKind::Highlight => {
                        let Some(bounds) = quads_bounds(&mark.quads) else {
                            continue;
                        };
                        let mut annotation =
                            target.create_highlight_annotation().map_err(mark_error)?;
                        annotation
                            .set_bounds(pdf_rect(bounds.0, bounds.1, bounds.2, bounds.3))
                            .map_err(mark_error)?;
                        append_attachment_points(annotation.attachment_points_mut(), &mark.quads)?;
                    }
                    AnnotKind::Underline => {
                        let Some(bounds) = quads_bounds(&mark.quads) else {
                            continue;
                        };
                        let mut annotation =
                            target.create_underline_annotation().map_err(mark_error)?;
                        annotation
                            .set_bounds(pdf_rect(bounds.0, bounds.1, bounds.2, bounds.3))
                            .map_err(mark_error)?;
                        append_attachment_points(annotation.attachment_points_mut(), &mark.quads)?;
                    }
                    AnnotKind::Strikeout => {
                        let Some(bounds) = quads_bounds(&mark.quads) else {
                            continue;
                        };
                        let mut annotation =
                            target.create_strikeout_annotation().map_err(mark_error)?;
                        annotation
                            .set_bounds(pdf_rect(bounds.0, bounds.1, bounds.2, bounds.3))
                            .map_err(mark_error)?;
                        append_attachment_points(annotation.attachment_points_mut(), &mark.quads)?;
                    }
                    AnnotKind::Note => {
                        if mark.text.trim().is_empty() {
                            continue;
                        }
                        let Some(first) = mark.quads.first() else {
                            continue;
                        };
                        let (left, _, _, top) = quad_bounds(first);
                        let mut annotation = target
                            .create_text_annotation(&mark.text)
                            .map_err(mark_error)?;
                        annotation
                            .set_bounds(pdf_rect(
                                left,
                                top - ANNOT_NOTE_SIDE,
                                left + ANNOT_NOTE_SIDE,
                                top,
                            ))
                            .map_err(mark_error)?;
                    }
                }
            }
        }
        // O Pdfium gera o /AP da anotação a cada criação, antes de o retângulo
        // e os quads serem definidos; regenerar aqui commita a aparência já
        // posicionada para os outros leitores.
        pdf_page.regenerate_content().map_err(mark_error)?;
    }

    document
        .save_to_bytes()
        .map_err(|e| EngineError(format!("{ANNOT_SAVE_FAILED}: {e}")))
}

#[cfg(test)]
/// Anotação lida de volta do documento: dados puros, sem handles do Pdfium —
/// atravessa o canal da worker para o teste de `save_copy`.
#[derive(Debug, Clone)]
pub(crate) struct StoredAnnotation {
    kind: AnnotKind,
    /// Um quad por ponto de anexo, na ordem (nota não tem: vetor vazio).
    quads: Vec<Quad>,
    text: String,
    /// `(left, bottom, right, top)` do `/Rect`, em espaço PDF.
    bounds: (f32, f32, f32, f32),
}

#[cfg(test)]
/// Lê as anotações de uma página do documento vivo, em ordem de criação.
/// Tipos que o `save_copy_from_doc` nunca grava são ignorados.
fn annotations_from_doc(
    document: &PdfDocument<'_>,
    page: PageNo,
) -> Result<Vec<StoredAnnotation>, EngineError> {
    let read_error = |e: PdfiumError| EngineError(format!("{ANNOT_READ_FAILED}: {e}"));
    let pdf_page = document
        .pages()
        .get(page_index(page)?)
        .map_err(read_error)?;
    let annotations = pdf_page.annotations();
    let mut out = Vec::with_capacity(annotations.len());
    for index in 0..annotations.len() {
        let annotation = annotations.get(index).map_err(read_error)?;
        let kind = match annotation.annotation_type() {
            PdfPageAnnotationType::Highlight => AnnotKind::Highlight,
            PdfPageAnnotationType::Underline => AnnotKind::Underline,
            PdfPageAnnotationType::Strikeout => AnnotKind::Strikeout,
            PdfPageAnnotationType::Text => AnnotKind::Note,
            _ => continue,
        };
        let points = annotation.attachment_points();
        let mut quads = Vec::with_capacity(points.len());
        for point in 0..points.len() {
            let quad = points.get(point).map_err(read_error)?;
            quads.push(Quad::from_rect(
                quad.left().value,
                quad.bottom().value,
                quad.right().value,
                quad.top().value,
            ));
        }
        let rect = annotation.bounds().map_err(read_error)?;
        out.push(StoredAnnotation {
            kind,
            quads,
            text: annotation.contents().unwrap_or_default(),
            bounds: (
                rect.left().value,
                rect.bottom().value,
                rect.right().value,
                rect.top().value,
            ),
        });
    }
    Ok(out)
}

fn mark_error(e: PdfiumError) -> EngineError {
    EngineError(format!("{ANNOT_MARK_FAILED}: {e}"))
}

/// Um ponto de anexo por `Quad` da sessão, na ordem — um retângulo por linha
/// marcada, como na tela.
fn append_attachment_points(
    points: &mut PdfPageAnnotationAttachmentPoints<'_>,
    quads: &[Quad],
) -> Result<(), EngineError> {
    for quad in quads {
        points
            .create_attachment_point_at_end(PdfQuadPoints::new_from_values(
                quad.x0, quad.y0, quad.x1, quad.y1, quad.x2, quad.y2, quad.x3, quad.y3,
            ))
            .map_err(mark_error)?;
    }
    Ok(())
}

/// `(left, bottom, right, top)` de um quad da sessão.
fn quad_bounds(quad: &Quad) -> (f32, f32, f32, f32) {
    let xs = [quad.x0, quad.x1, quad.x2, quad.x3];
    let ys = [quad.y0, quad.y1, quad.y2, quad.y3];
    (
        xs.iter().fold(f32::INFINITY, |a, &b| a.min(b)),
        ys.iter().fold(f32::INFINITY, |a, &b| a.min(b)),
        xs.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b)),
        ys.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b)),
    )
}

/// `/Rect` do Pdfium a partir de `(left, bottom, right, top)` em espaço PDF.
/// A lib inverte a ordem na assinatura (`bottom, left, top, right`).
fn pdf_rect(left: f32, bottom: f32, right: f32, top: f32) -> PdfRect {
    PdfRect::new_from_values(bottom, left, top, right)
}

/// União dos quads da marcação (`/Rect` da anotação); `None` sem quads.
fn quads_bounds(quads: &[Quad]) -> Option<(f32, f32, f32, f32)> {
    quads.iter().map(quad_bounds).reduce(|acc, rect| {
        (
            acc.0.min(rect.0),
            acc.1.min(rect.1),
            acc.2.max(rect.2),
            acc.3.max(rect.3),
        )
    })
}

/// Quartos de volta horários da vista → rotação do Pdfium (também horária).
fn rotation_for(quarter_turns: u8) -> PdfPageRenderRotation {
    match quarter_turns & 3 {
        0 => PdfPageRenderRotation::None,
        1 => PdfPageRenderRotation::Degrees90,
        2 => PdfPageRenderRotation::Degrees180,
        _ => PdfPageRenderRotation::Degrees270,
    }
}

fn pdfium_library_candidates() -> Vec<PathBuf> {
    std::env::current_exe()
        .ok()
        .map(|exe| pdfium_candidates_for(&exe))
        .unwrap_or_default()
}

fn pdfium_candidates_for(exe: &Path) -> Vec<PathBuf> {
    let mut out = vec![frameworks_lib_path(exe)];
    if let Some(dir) = exe.parent() {
        out.push(dir.join(Pdfium::pdfium_platform_library_name()));
    }
    out
}

fn frameworks_lib_path(exe: &Path) -> PathBuf {
    let dir = exe.parent().unwrap_or_else(|| Path::new(""));
    dir.join("../Frameworks")
        .join(Pdfium::pdfium_platform_library_name())
}

fn text_layer_from_page(page: &PdfPage<'_>, page_no: PageNo) -> Result<TextLayer, EngineError> {
    let text = match page.text() {
        Ok(text) => text,
        Err(_) => {
            return Ok(TextLayer {
                page: page_no,
                plain: String::new(),
                glyphs: Vec::new(),
            });
        }
    };
    let chars = text.chars();

    let mut plain = String::new();
    let mut glyphs = Vec::new();
    for ch in chars.iter() {
        let cluster: String = ch.unicode_string().unwrap_or_default().nfc().collect();
        if cluster.is_empty() {
            continue;
        }
        let quad = quad_from_char(&ch);
        plain.push_str(&cluster);
        glyphs.push(Glyph { cluster, quad });
    }
    let plain: String = plain.nfc().collect();
    Ok(TextLayer {
        page: page_no,
        plain,
        glyphs,
    })
}

fn quad_from_char(ch: &PdfPageTextChar<'_>) -> Quad {
    match ch.tight_bounds() {
        Ok(rect) => Quad::from_rect(
            rect.left().value,
            rect.bottom().value,
            rect.right().value,
            rect.top().value,
        ),
        Err(_) => Quad::from_rect(0.0, 0.0, 0.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TextRange;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn rotation_for_maps_quarter_turns_clockwise() {
        assert_eq!(rotation_for(0), PdfPageRenderRotation::None);
        assert_eq!(rotation_for(1), PdfPageRenderRotation::Degrees90);
        assert_eq!(rotation_for(2), PdfPageRenderRotation::Degrees180);
        assert_eq!(rotation_for(3), PdfPageRenderRotation::Degrees270);
        assert_eq!(rotation_for(5), PdfPageRenderRotation::Degrees90);
    }

    #[test]
    fn engine_handle_is_send_sync_for_tasks() {
        // A sessão move clones do engine para `Task::perform` + `spawn_blocking`.
        assert_send_sync::<PdfiumEngine>();
    }

    /// Fixture de `scripts/generate_samples.py` (`public/samples/` é read-only:
    /// o PDF nunca é editado à mão).
    fn sample_engine(name: &str) -> Option<PdfiumEngine> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../public/samples")
            .join(name);
        let bytes = std::fs::read(path).ok()?;
        PdfiumEngine::open(Arc::from(bytes.as_slice())).ok()
    }

    #[test]
    fn outline_reads_nested_bookmarks_with_pages() {
        let Some(engine) = sample_engine("sumario-folio.pdf") else {
            return; // sem Pdfium ao lado do binário de teste
        };
        let outline = engine
            .outline()
            .expect("leitura do outline")
            .expect("o fixture declara bookmarks");
        assert_eq!(outline.items.len(), 16);
        assert_eq!(outline.items[0].title, "1. Identificação das partes");
        assert_eq!(outline.items[0].page, PageNo::first());
        assert!(outline.items[0].children.is_empty());
        // Nó com filhos: título, página (0-based) e ordem dos filhos.
        let precos = &outline.items[3];
        assert_eq!(precos.title, "4. Preços e reajuste");
        assert_eq!(precos.page, PageNo::from_index(3));
        assert_eq!(precos.children.len(), 2);
        assert_eq!(precos.children[0].title, "4.1 Reajuste anual");
        assert_eq!(precos.children[1].title, "4.2 Revisão extraordinária");
        assert_eq!(precos.children[1].page, PageNo::from_index(3));
    }

    #[test]
    fn outline_is_none_for_pdf_without_bookmarks() {
        let Some(engine) = sample_engine("guia-folio.pdf") else {
            return;
        };
        // `None`, não árvore vazia: a aba Sumário só existe quando há outline.
        assert!(engine.outline().expect("leitura do outline").is_none());
    }

    #[test]
    fn frameworks_path_points_at_bundle_lib() {
        let exe = Path::new("/Applications/TsuroPDF.app/Contents/MacOS/TsuroPDF");
        let got = frameworks_lib_path(exe);
        assert_eq!(
            got.parent().and_then(|p| p.file_name()),
            Some(std::ffi::OsStr::new("Frameworks"))
        );
        assert_eq!(
            got.file_name(),
            Some(Pdfium::pdfium_platform_library_name().as_os_str())
        );
    }

    #[test]
    fn pdfium_candidates_stay_next_to_the_binary() {
        // Binário real do teste: absoluto em qualquer plataforma (um caminho
        // estilo macOS não é absoluto no Windows e quebrava este teste lá).
        let exe = std::env::current_exe().expect("test binary path");
        let dir = exe.parent().expect("test binary dir");
        let got = pdfium_candidates_for(&exe);
        assert!(got.iter().all(|p| p != Path::new(".") && p.is_absolute()));
        assert!(got.iter().any(|p| p
            .parent()
            .and_then(|d| d.file_name())
            .is_some_and(|n| n == "Frameworks")));
        assert!(got.iter().any(|p| p.parent() == Some(dir)));
    }

    #[test]
    fn open_propagates_worker_errors_without_hanging() {
        // Bytes vazios nunca abrem: sem Pdfium, falha no bind; com Pdfium,
        // falha no parse. Em ambos os casos o handshake da worker responde.
        let bytes: Arc<[u8]> = Arc::from(Vec::new());
        let result = PdfiumEngine::open(bytes);
        assert!(result.is_err(), "bytes vazios devem falhar");
    }

    #[test]
    fn pdfium_binds_when_ci_requires_it() {
        // Pela worker estática: um segundo `Pdfium::new` no processo travaria
        // no mutex global do pdfium-render (`InitLibrary` retém o lock) — o
        // gate abre um documento em vez de dar bind de novo.
        let Some(bytes) = sample_pdf_bytes() else {
            return;
        };
        match PdfiumEngine::open(bytes) {
            Ok(_) => {}
            Err(e) if std::env::var("CI").is_ok() => panic!("{e}"),
            Err(_) => {}
        }
    }

    fn sample_pdf_bytes() -> Option<Arc<[u8]>> {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../public/samples/guia-folio.pdf");
        std::fs::read(path).ok().map(Arc::from)
    }

    /// Salva uma cópia com um highlight e uma nota reais (quads do text layer)
    /// e confere o read-back: mesmas coordenadas do texto, sem flip de Y.
    #[test]
    fn save_copy_writes_markup_and_note_in_session_coordinates() {
        let Some(bytes) = sample_pdf_bytes() else {
            return;
        };
        let Ok(engine) = PdfiumEngine::open(bytes) else {
            return;
        };
        let page = PageNo::first();
        let (media, text) = engine.page_data(page).expect("page data");
        let quads: Vec<Quad> = text.glyphs.iter().take(3).map(|g| g.quad).collect();
        let Some(first) = quads.first().copied() else {
            return;
        };
        let annotations = vec![
            Annotation {
                id: 1,
                page,
                range: TextRange {
                    start: 0,
                    end: quads.len(),
                },
                quads: quads.clone(),
                kind: AnnotKind::Highlight,
                text: String::new(),
            },
            Annotation {
                id: 2,
                page,
                range: TextRange { start: 0, end: 1 },
                quads: vec![first],
                kind: AnnotKind::Note,
                text: "nota do teste".into(),
            },
        ];

        let saved = engine.save_copy(&annotations).expect("cópia marcada");
        assert!(!saved.is_empty(), "cópia marcada não pode ser vazia");
        let pages = engine.page_count();
        engine.close();
        drop(engine);

        // Read-back pela worker: a cópia é reaberta como um novo documento
        // servido pelo mesmo `Pdfium` — um segundo `bind()` no processo
        // travaria para sempre no mutex global da biblioteca.
        let reopened =
            PdfiumEngine::open(Arc::from(saved)).expect("o motor precisa reabrir a cópia salva");
        assert_eq!(reopened.page_count(), pages);
        let (re_media, _) = reopened.page_data(page).expect("media da cópia");
        assert!((re_media.width - media.width).abs() < 0.5);
        assert!((re_media.height - media.height).abs() < 0.5);

        let stored = reopened.annotations(page).expect("anotações da cópia");
        assert_eq!(stored.len(), 2);
        let highlight = &stored[0];
        assert!(matches!(highlight.kind, AnnotKind::Highlight));
        assert_eq!(highlight.quads.len(), quads.len(), "um ponto por quad");
        for (index, (got, want)) in highlight.quads.iter().zip(quads.iter()).enumerate() {
            let (left, bottom, right, top) = quad_bounds(want);
            let (got_left, got_bottom, got_right, got_top) = quad_bounds(got);
            assert!(
                (got_left - left).abs() < 0.05 && (got_right - right).abs() < 0.05,
                "quad {index} horizontal: {got:?} vs {want:?}"
            );
            assert!(
                (got_bottom - bottom).abs() < 0.05 && (got_top - top).abs() < 0.05,
                "quad {index} vertical: {got:?} vs {want:?}"
            );
        }

        let note = &stored[1];
        assert!(matches!(note.kind, AnnotKind::Note));
        assert_eq!(note.text, "nota do teste");
        let (left, _, _, top) = quad_bounds(&first);
        assert!((note.bounds.0 - left).abs() < 0.05);
        assert!((note.bounds.3 - top).abs() < 0.05);
    }

    #[test]
    fn save_copy_without_annotations_keeps_the_document() {
        let Some(bytes) = sample_pdf_bytes() else {
            return;
        };
        let Ok(engine) = PdfiumEngine::open(bytes) else {
            return;
        };
        let pages = engine.page_count();
        let saved = engine.save_copy(&[]).expect("cópia limpa");
        drop(engine);
        let Ok(reopened) = PdfiumEngine::open(Arc::from(saved)) else {
            panic!("cópia limpa precisa reabrir");
        };
        assert_eq!(reopened.page_count(), pages);
    }

    #[test]
    fn save_copy_rejects_page_outside_the_document() {
        let Some(bytes) = sample_pdf_bytes() else {
            return;
        };
        let Ok(engine) = PdfiumEngine::open(bytes) else {
            return;
        };
        let err = engine
            .save_copy(&[Annotation {
                id: 1,
                page: PageNo::from_index(engine.page_count()),
                range: TextRange { start: 0, end: 1 },
                quads: Vec::new(),
                kind: AnnotKind::Highlight,
                text: String::new(),
            }])
            .unwrap_err();
        assert_eq!(err.0, ANNOT_PAGE_OUT_OF_RANGE);
    }

    fn assert_too_large(page_w: f32, page_h: f32, factor: f32) {
        let err = render_target_px(page_w, page_h, factor).unwrap_err();
        assert_eq!(err.0, RENDER_TOO_LARGE);
    }

    fn assert_invalid(page_w: f32, page_h: f32, factor: f32) {
        let err = render_target_px(page_w, page_h, factor).unwrap_err();
        assert_eq!(err.0, RENDER_INVALID);
    }

    #[test]
    fn render_target_rejects_narrow_tall_page() {
        // Largura abaixo do teto; altura estoura o lado.
        assert_too_large(10.0, 100_000.0, 1.0);
    }

    #[test]
    fn render_target_rejects_wide_short_page() {
        assert_too_large(100_000.0, 10.0, 1.0);
    }

    #[test]
    fn render_target_rejects_pixel_cap_even_when_sides_fit() {
        // 8001×8000 = 64_008_000 > 64M, ambos os lados < 16384.
        assert_too_large(8_001.0, 8_000.0, 1.0);
    }

    #[test]
    fn render_target_accepts_limits() {
        let side = render_target_px(MAX_RENDER_SIDE as f32, 1.0, 1.0).unwrap();
        assert_eq!(side.width, MAX_RENDER_SIDE as i32);
        assert_eq!(side.height, 1);

        let pixels = render_target_px(8_000.0, 8_000.0, 1.0).unwrap();
        assert_eq!(pixels.width, 8_000);
        assert_eq!(pixels.height, 8_000);

        // A4 a 8×, zoom máximo da UI, tem de caber.
        let a4 = render_target_px(595.0, 842.0, 8.0).unwrap();
        assert_eq!(a4.width, 4_760);
        assert_eq!(a4.height, 6_736);
    }

    #[test]
    fn render_target_rejects_non_finite_and_non_positive() {
        assert_invalid(f32::NAN, 100.0, 1.0);
        assert_invalid(100.0, f32::NAN, 1.0);
        assert_invalid(100.0, 100.0, f32::NAN);
        assert_invalid(f32::INFINITY, 100.0, 1.0);
        assert_invalid(100.0, f32::NEG_INFINITY, 1.0);
        assert_invalid(100.0, 100.0, 0.0);
        assert_invalid(0.0, 100.0, 1.0);
        assert_invalid(100.0, 0.0, 1.0);
        assert_invalid(-10.0, 100.0, 1.0);
        assert_invalid(100.0, -10.0, 1.0);
        assert_invalid(100.0, 100.0, -1.0);
        // Produto explode para inf sem que cada argumento seja inf.
        assert_invalid(1e30, 1.0, 1e10);
    }

    #[test]
    fn render_target_rejects_subnormal_page_that_overflows_scale() {
        // 1×1 passaria no teto de px, mas target/page_w em f32 é inf.
        assert_invalid(1e-40, 1e-40, 1.0);
        assert_invalid(1e-40, 100.0, 1.0);
        assert_invalid(100.0, 1e-40, 1.0);
    }
}
