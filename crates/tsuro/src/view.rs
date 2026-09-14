use iced::widget::canvas::event::{Event as CanvasEvent, Status as CanvasStatus};
use iced::widget::canvas::{Frame, Geometry, Path, Program, Stroke, Style};
use iced::widget::{
    button, column, container, image, mouse_area, pick_list, row, scrollable, stack, svg, text,
    text_input, tooltip, Canvas, Space,
};
use iced::{mouse, Alignment, Background, Border, Color, Element, Length, Padding, Point};
use iced::{Rectangle, Size};
use tsuro_sign::SignatureStatus;

use crate::browse::{EmptyState, FsEntry};
use crate::kiri::{self, Theme, Tokens};
use crate::page::{MediaBox, PageNo};
use crate::print::{PrintOrientation, MAX_COPIES};
use crate::session::{
    display_rect, page_pt_at, AnnotKind, Message, NavCmd, NoteDraft, PrintDialog, RangeMode, Ready,
    Session, ViewMode, Zoom, ZoomFactor, DOC_GAP, DOC_PAD_BOTTOM, DOC_PAD_TOP, DOC_PAD_X,
    PAGES_PANEL_W, SIG_PANEL_W, THUMB_ROW,
};

/// Altura do chrome Kiri: toolbar 36px + progresso 2px + respiro.
pub const CHROME_HEIGHT: f32 = 46.0;

pub fn pages_scroll_id() -> scrollable::Id {
    scrollable::Id::new("tsuro-pages")
}

pub fn doc_scroll_id() -> scrollable::Id {
    scrollable::Id::new("tsuro-doc")
}

pub fn chrome(session: &Session, theme: Theme) -> Element<'_, Message> {
    let t = Tokens::for_theme(theme);
    let body: Element<'_, Message> = match session {
        Session::Empty(empty) => empty_browser(empty, t),
        Session::Loading { source, .. } => {
            text(format!("Abrindo {}…", source.path().display())).into()
        }
        Session::Failed { message, .. } => column![
            text("Não foi possível abrir o documento").size(20),
            text(message),
        ]
        .spacing(8)
        .into(),
        Session::Ready(ready) => ready_body(ready, t),
    };

    // Toolbar 36px + progresso 2px + respiro 8px = `CHROME_HEIGHT` (46px).
    let mut col = column![topbar(session, t)];
    if let Session::Ready(ready) = session {
        col = col.push(progress(ready, t));
    }
    col = col.push(body);
    let main = container(
        col.spacing(0)
            .padding(4)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| container::Style {
        background: Some(Background::Color(t.bg)),
        text_color: Some(t.ink),
        ..container::Style::default()
    });
    match session {
        // Modal de impressão captura tudo; menu ⋯ nunca abre junto (fecha ao abrir).
        Session::Ready(ready) if ready.print_dialog.is_some() => {
            let dialog = ready.print_dialog.as_ref().expect("checked above");
            stack![main, print_layer(ready, dialog, t)].into()
        }
        // Popover de nota (issue #30): captura tudo, como o modal de impressão.
        Session::Ready(ready) if ready.note_draft.is_some() => {
            let draft = ready.note_draft.as_ref().expect("checked above");
            stack![main, note_layer(draft, t)].into()
        }
        // Aviso de documento assinado (⋯ → Salvar cópia): captura tudo.
        Session::Ready(ready) if ready.save_warning => stack![main, save_warning_layer(t)].into(),
        // Overlay visual: só os botões capturam clique, o resto atravessa.
        Session::Ready(ready) if ready.overflow_open => {
            stack![main, overflow_layer(ready, t)].into()
        }
        _ => main.into(),
    }
}

// Lucide restante (sem par Ori v1): `copy`, `x`, `folder`, `file-text`, `chevron-left`
// (empty state). Toolbar usa `kiri::ori!` — cor fixa no SVG, sem `.style()`.
macro_rules! icon {
    ($t:expr, $file:literal) => {
        svg(svg::Handle::from_memory(include_bytes!(concat!(
            "../assets/icons/",
            $file,
            ".svg"
        ))))
        .width(Length::Fixed(17.0))
        .height(Length::Fixed(17.0))
        .style(move |_theme, _status| svg::Style {
            color: Some($t.ink),
        })
    };
}

fn control_style(
    t: Tokens,
    active: bool,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    kiri::ibtn_style(t, active)
}

fn control(
    t: Tokens,
    btn: iced::widget::button::Button<'_, Message>,
) -> iced::widget::button::Button<'_, Message> {
    control_active(t, btn, false)
}

fn control_active(
    t: Tokens,
    btn: iced::widget::button::Button<'_, Message>,
    active: bool,
) -> iced::widget::button::Button<'_, Message> {
    btn.padding(Padding::from([9, 10]))
        .style(control_style(t, active))
}

/// Botão-ícone 30×30 para dentro de segmentos (`kiri::seg_style` + padding 2).
fn control_seg(
    t: Tokens,
    btn: iced::widget::button::Button<'_, Message>,
) -> iced::widget::button::Button<'_, Message> {
    btn.padding(Padding::from([5, 6]))
        .style(control_style(t, false))
}

fn tip<'a>(content: impl Into<Element<'a, Message>>, label: &'static str) -> Element<'a, Message> {
    tooltip::Tooltip::new(content, text(label).size(13), tooltip::Position::Bottom).into()
}

fn open_button(t: Tokens) -> Element<'static, Message> {
    tip(
        control(
            t,
            button(kiri::ori!("folder-open")).on_press(Message::PickFile),
        ),
        "Abrir PDF",
    )
}

fn home_button(t: Tokens) -> Element<'static, Message> {
    tip(
        control(t, button(kiri::ori!("home")).on_press(Message::Close)),
        "Início",
    )
}

/// Moldura da toolbar Kiri: 36px, fundo `chrome`, respiro horizontal 8px.
fn toolbar_frame(t: Tokens, content: Element<'_, Message>) -> Element<'_, Message> {
    container(content)
        .width(Length::Fill)
        .height(Length::Fixed(36.0))
        .padding(Padding::from([0, 8]))
        .style(move |_| container::Style {
            background: Some(Background::Color(t.chrome)),
            ..container::Style::default()
        })
        .into()
}

/// Barra única Kiri (`Session::Ready`): abrir │ pílula │ zoom │ painéis │ ⋯.
fn topbar(session: &Session, t: Tokens) -> Element<'_, Message> {
    if let Session::Ready(ready) = session {
        let n = ready.page_count().max(1);
        let pill = container(
            row![
                kiri::ori_small!("search"),
                text_input("Buscar", ready.search.query())
                    .on_input(Message::SearchChanged)
                    .width(Length::Fixed(200.0)),
                row![
                    tip(
                        text_input("Página", ready.page_input())
                            .on_input(Message::PageInput)
                            .on_submit(Message::PageSubmit)
                            .width(Length::Fixed(48.0))
                            .padding([4, 6])
                            .size(12),
                        "Ir para página (Enter confirma)",
                    ),
                    text(format!("/{n}")).size(12).color(t.muted),
                ]
                .spacing(4)
                .align_y(Alignment::Center),
                container(
                    row![
                        tip(
                            control_seg(
                                t,
                                button(kiri::ori!("chevron-left"))
                                    .on_press(Message::Nav(NavCmd::Previous))
                            ),
                            "Página anterior"
                        ),
                        tip(
                            control_seg(
                                t,
                                button(kiri::ori!("chevron-right"))
                                    .on_press(Message::Nav(NavCmd::Next))
                            ),
                            "Próxima página"
                        ),
                    ]
                    .spacing(0)
                    .align_y(Alignment::Center)
                )
                .padding(2)
                .style(kiri::seg_style(t)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(Padding {
            top: 4.0,
            right: 6.0,
            bottom: 4.0,
            left: 12.0,
        })
        .style(kiri::pill_style(t))
        .max_width(520.0);
        let current = match ready.zoom {
            Zoom::Manual(z) => z.get(),
            Zoom::Width | Zoom::Page => ready
                .zoom
                .scale(ready.viewport(), ready.media(ready.visible))
                .factor(),
        };
        let out = Zoom::Manual(ZoomFactor::new(current / 1.1));
        let into = Zoom::Manual(ZoomFactor::new(current * 1.1));

        let zoom_seg = container(
            row![
                tip(
                    control_seg(
                        t,
                        button(kiri::ori!("minus")).on_press(Message::SetZoom(out))
                    ),
                    "Diminuir zoom"
                ),
                tip(
                    control_seg(
                        t,
                        button(kiri::ori!("fit-width")).on_press(Message::SetZoom(Zoom::Width))
                    ),
                    "Ajustar à largura"
                ),
                tip(
                    control_seg(
                        t,
                        button(kiri::ori!("plus")).on_press(Message::SetZoom(into))
                    ),
                    "Aumentar zoom"
                ),
            ]
            .spacing(0)
            .align_y(Alignment::Center),
        )
        .padding(2)
        .style(kiri::seg_style(t));

        // Copiar/fechar/fit-page vivem no menu ⋯ (`overflow_menu`).
        let shield = control_active(
            t,
            button(kiri::ori!("shield")).on_press(Message::ToggleSignatures),
            ready.signatures_open,
        );
        let dot = kiri::status_dot_color(t, ready.signatures.signatures.iter().map(|s| s.status));
        let shield_el: Element<'_, Message> = match dot {
            Some(dot) => tip(
                stack![
                    shield,
                    container(
                        container(Space::with_width(Length::Fixed(7.0)))
                            .width(Length::Fixed(7.0))
                            .height(Length::Fixed(7.0))
                            .style(move |_| container::Style {
                                background: Some(Background::Color(dot)),
                                border: Border {
                                    color: t.chrome,
                                    width: 2.0,
                                    radius: 99.0.into(),
                                },
                                ..container::Style::default()
                            })
                    )
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Alignment::End)
                    .align_y(Alignment::Start)
                    .padding(Padding {
                        top: 6.0,
                        right: 6.0,
                        bottom: 0.0,
                        left: 0.0,
                    })
                ],
                "Assinaturas",
            ),
            None => tip(shield, "Assinaturas"),
        };
        let mut right = row![
            tip(
                control_active(
                    t,
                    button(kiri::ori!("pages")).on_press(Message::TogglePages),
                    ready.pages_open
                ),
                "Páginas"
            ),
            shield_el,
        ]
        .spacing(4)
        .align_y(Alignment::Center);
        right = right.push(tip(
            control_active(
                t,
                button(kiri::ori!("more")).on_press(Message::ToggleOverflow),
                ready.overflow_open,
            ),
            "Mais opções",
        ));

        // Ferramentas de marcar na cara (seleção ativa): ⋯ é fuga, não casa.
        if ready.selection_plain_text().is_some() {
            let mark_seg = container(
                row![
                    tip(
                        control_seg(
                            t,
                            button(text("Destacar").size(12))
                                .on_press(Message::Annotate(AnnotKind::Highlight))
                        ),
                        "Destacar (H)"
                    ),
                    tip(
                        control_seg(
                            t,
                            button(text("Sublinhar").size(12))
                                .on_press(Message::Annotate(AnnotKind::Underline))
                        ),
                        "Sublinhar (U)"
                    ),
                    tip(
                        control_seg(
                            t,
                            button(text("Riscar").size(12))
                                .on_press(Message::Annotate(AnnotKind::Strikeout))
                        ),
                        "Riscar (S)"
                    ),
                    tip(
                        control_seg(
                            t,
                            button(text("Nota (N)").size(12))
                                .on_press(Message::Annotate(AnnotKind::Note))
                        ),
                        "Nota (N)"
                    ),
                ]
                .spacing(0)
                .align_y(Alignment::Center),
            )
            .padding(2)
            .style(kiri::seg_style(t));
            right = row![mark_seg, kiri::vsep(t), right,]
                .spacing(4)
                .align_y(Alignment::Center);
        }

        return toolbar_frame(
            t,
            row![
                home_button(t),
                open_button(t),
                kiri::vsep(t),
                Space::with_width(Length::Fill),
                pill,
                Space::with_width(Length::Fill),
                kiri::vsep(t),
                zoom_seg,
                kiri::vsep(t),
                right,
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into(),
        );
    }

    // Tela inicial não precisa de home; erro/carregando usam para voltar.
    let mut items: Vec<Element<'_, Message>> = Vec::new();
    if !matches!(session, Session::Empty(_)) {
        items.push(home_button(t));
    }
    items.push(open_button(t));
    toolbar_frame(t, row(items).spacing(4).align_y(Alignment::Center).into())
}

/// Camada do menu ⋯: ocupa tudo mas só os botões capturam clique.
fn overflow_layer(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    container(overflow_menu(ready, t))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::End)
        .align_y(Alignment::Start)
        .padding(Padding {
            top: 44.0,
            right: 8.0,
            bottom: 0.0,
            left: 0.0,
        })
        .into()
}

/// Menu ⋯ (PR 4): zoom página, copiar, fechar, aparência.
fn overflow_menu(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let mut items = column![].spacing(2).width(Length::Fill);
    items = items.push(menu_item(
        t,
        "Ajustar página inteira",
        Message::SetZoom(Zoom::Page),
    ));
    items = items.push(menu_item(t, "Girar vista (90°)", Message::RotateView));
    items = items.push(print_menu_item(t));
    if ready.can_history_back() {
        items = items.push(menu_item(t, "Voltar", Message::HistoryBack));
    }
    if ready.can_history_forward() {
        items = items.push(menu_item(t, "Avançar", Message::HistoryForward));
    }
    items = items.push(text("Modo de página").size(12).color(t.muted));
    let single = ready.view_mode == ViewMode::Single;
    items = items.push(menu_item(
        t,
        if single {
            "● Página única"
        } else {
            "○ Página única"
        },
        Message::SetViewMode(ViewMode::Single),
    ));
    items = items.push(menu_item(
        t,
        if single {
            "○ Rolagem contínua"
        } else {
            "● Rolagem contínua"
        },
        Message::SetViewMode(ViewMode::Continuous),
    ));
    if ready.selection_plain_text().is_some() {
        items = items.push(menu_item(t, "Copiar seleção", Message::CopySelection));
    }
    if ready.can_annot_undo() {
        items = items.push(menu_item(t, "Desfazer marcação", Message::AnnotUndo));
    }
    if ready.can_annot_redo() {
        items = items.push(menu_item(t, "Refazer marcação", Message::AnnotRedo));
    }
    if !ready.annotations.is_empty() {
        items = items.push(menu_item(
            t,
            "Salvar cópia com marcações…",
            Message::SaveCopyRequested,
        ));
    }
    items = items.push(menu_item(t, "Fechar documento", Message::Close));
    items = items.push(
        container(Space::with_height(Length::Fixed(1.0)))
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(t.line)),
                ..container::Style::default()
            }),
    );
    items = items.push(text("Aparência").size(12).color(t.muted));
    let dark = ready.theme.is_dark();
    items = items.push(
        row![
            menu_theme_button(t, "Escuro", Theme::Dark, dark),
            menu_theme_button(t, "Claro", Theme::Light, !dark),
        ]
        .spacing(4),
    );
    container(items)
        .width(Length::Fixed(232.0))
        .padding(6)
        .style(kiri::menu_style(t))
        .into()
}

fn menu_item(t: Tokens, label: &'static str, message: Message) -> Element<'static, Message> {
    button(text(label).size(13))
        .width(Length::Fill)
        .padding(Padding::from([8, 10]))
        .style(kiri::menu_item_style(t))
        .on_press(message)
        .into()
}

/// ⋯ → Imprimir: abre o diálogo próprio.
fn print_menu_item(t: Tokens) -> Element<'static, Message> {
    button(
        row![kiri::ori!("print"), text("Imprimir").size(13)]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(Padding::from([8, 10]))
    .style(kiri::menu_item_style(t))
    .on_press(Message::OpenPrintDialog)
    .into()
}

/// Modal de impressão: fundo fecha ao clicar, cartão captura sem efeito —
/// nada atravessa para o documento atrás.
fn print_layer<'a>(ready: &'a Ready, dialog: &'a PrintDialog, t: Tokens) -> Element<'a, Message> {
    let dim = container(Space::with_width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.55))),
            ..container::Style::default()
        });
    let card = container(mouse_area(print_card(ready, dialog, t)).on_press(Message::PrintNop))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
    stack![mouse_area(dim).on_press(Message::ClosePrintDialog), card,].into()
}

fn print_card<'a>(ready: &'a Ready, dialog: &'a PrintDialog, t: Tokens) -> Element<'a, Message> {
    let pages = dialog.preview_pages(ready.page_count(), ready.visible);
    let at = dialog.preview.min(pages.len().saturating_sub(1));
    let mut col = column![
        row![
            text("Imprimir").size(16),
            Space::with_width(Length::Fill),
            button(text("Fechar").size(13))
                .style(kiri::menu_item_style(t))
                .on_press_maybe((!dialog.busy).then_some(Message::ClosePrintDialog)),
        ]
        .align_y(Alignment::Center),
        row![
            print_preview(ready, dialog, &pages, at, t),
            print_controls(dialog, t),
        ]
        .spacing(16),
    ]
    .spacing(12);
    if let Some(err) = &dialog.error {
        col = col.push(
            text(format!("Não foi possível imprimir: {err}"))
                .size(13)
                .color(t.danger),
        );
    }
    col = col.push(print_footer(dialog, t));
    container(col)
        .width(Length::Fixed(620.0))
        .padding(16)
        .style(kiri::menu_style(t))
        .into()
}

/// Preview reaproveita o thumb do cache (escala de tela, sem render novo).
fn print_preview<'a>(
    ready: &'a Ready,
    dialog: &'a PrintDialog,
    pages: &[PageNo],
    at: usize,
    t: Tokens,
) -> Element<'a, Message> {
    let thumb: Element<'a, Message> =
        match pages.get(at).and_then(|page| ready.thumb_surface(*page)) {
            Some(surface) => image(surface.image.clone())
                .width(Length::Fixed(220.0))
                .into(),
            None => container(text("carregando…").size(13).color(t.muted))
                .width(Length::Fixed(220.0))
                .height(Length::Fixed(280.0))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into(),
        };
    let pager = row![
        button(kiri::ori!("chevron-left"))
            .style(kiri::ibtn_style(t, false))
            .on_press_maybe((!dialog.busy && at > 0).then_some(Message::PrintPreviewPrev)),
        text(format!("{} de {}", at + 1, pages.len().max(1))).size(13),
        button(kiri::ori!("chevron-right"))
            .style(kiri::ibtn_style(t, false))
            .on_press_maybe(
                (!dialog.busy && at + 1 < pages.len()).then_some(Message::PrintPreviewNext)
            ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    column![thumb, pager]
        .spacing(8)
        .align_x(Alignment::Center)
        .into()
}

fn print_controls(dialog: &PrintDialog, t: Tokens) -> Element<'_, Message> {
    let busy = dialog.busy;
    let printer: Element<'_, Message> = if dialog.printers_loading {
        text("Carregando impressoras…")
            .size(13)
            .color(t.muted)
            .into()
    } else if dialog.printers.is_empty() {
        text("Nenhuma impressora encontrada")
            .size(13)
            .color(t.warn)
            .into()
    } else {
        let names: Vec<String> = dialog.printers.iter().map(|p| p.name.clone()).collect();
        let selected = dialog.selected_printer().map(|p| p.name.clone());
        pick_list(names.clone(), selected, move |name: String| {
            Message::PrintSelectPrinter(names.iter().position(|n| *n == name).unwrap_or(0))
        })
        .placeholder("Impressora")
        .width(Length::Fill)
        .into()
    };
    let mut col = column![
        section_title("Impressora", t),
        printer,
        section_title("Páginas", t),
        row![
            seg_button(t, "Todas", RangeMode::All, dialog, busy),
            seg_button(t, "Atual", RangeMode::Current, dialog, busy),
            seg_button(t, "De–Até", RangeMode::Custom, dialog, busy),
        ]
        .spacing(4),
    ]
    .spacing(6);
    if dialog.range_mode == RangeMode::Custom {
        col = col.push(
            row![
                text_input("De", &dialog.from_input)
                    .on_input(Message::PrintSetFromInput)
                    .width(Length::Fixed(64.0)),
                text("até").size(13).color(t.muted),
                text_input("Até", &dialog.to_input)
                    .on_input(Message::PrintSetToInput)
                    .width(Length::Fixed(64.0)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    col = col.push(section_title("Cópias", t));
    col = col.push(
        row![
            button(kiri::ori!("minus"))
                .style(kiri::ibtn_style(t, false))
                .on_press_maybe((!busy && dialog.copies > 1).then_some(Message::PrintCopiesMinus)),
            container(text(dialog.copies.to_string()).size(14))
                .width(Length::Fixed(32.0))
                .align_x(Alignment::Center),
            button(kiri::ori!("plus"))
                .style(kiri::ibtn_style(t, false))
                .on_press_maybe(
                    (!busy && dialog.copies < MAX_COPIES).then_some(Message::PrintCopiesPlus)
                ),
        ]
        .spacing(4)
        .align_y(Alignment::Center),
    );
    col = col.push(section_title("Orientação", t));
    col = col.push(
        row![
            ori_button(t, "Automática", PrintOrientation::Auto, dialog, busy),
            ori_button(t, "Retrato", PrintOrientation::Portrait, dialog, busy),
            ori_button(t, "Paisagem", PrintOrientation::Landscape, dialog, busy),
        ]
        .spacing(4),
    );
    col.width(Length::Fill).into()
}

fn seg_button(
    t: Tokens,
    label: &'static str,
    mode: RangeMode,
    dialog: &PrintDialog,
    busy: bool,
) -> Element<'static, Message> {
    button(text(label).size(13))
        .style(kiri::ibtn_style(t, dialog.range_mode == mode))
        .on_press_maybe((!busy).then_some(Message::PrintSetRangeMode(mode)))
        .into()
}

fn ori_button(
    t: Tokens,
    label: &'static str,
    orientation: PrintOrientation,
    dialog: &PrintDialog,
    busy: bool,
) -> Element<'static, Message> {
    button(text(label).size(13))
        .style(kiri::ibtn_style(t, dialog.orientation == orientation))
        .on_press_maybe((!busy).then_some(Message::PrintSetOrientation(orientation)))
        .into()
}

fn print_footer(dialog: &PrintDialog, t: Tokens) -> Element<'static, Message> {
    let busy = dialog.busy;
    let label = if busy { "Enviando…" } else { "Imprimir" };
    let secondary = |label: &'static str, message: Message| {
        button(text(label).size(13))
            .padding(Padding::from([8, 12]))
            .style(kiri::menu_item_style(t))
            .on_press_maybe((!busy).then_some(message))
    };
    row![
        Space::with_width(Length::Fill),
        secondary("Abrir PDF", Message::PrintOpenPdf),
        secondary("Cancelar", Message::ClosePrintDialog),
        button(text(label).size(13))
            .padding(Padding::from([8, 12]))
            .style(kiri::menu_item_style(t))
            .on_press_maybe((!busy).then_some(Message::PrintSubmit)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// Popover de nota (issue #30): mesmo padrão do modal de impressão — fundo
/// escurece e cancela no clique, cartão engole o clique (`PrintNop` é no-op).
fn note_layer(draft: &NoteDraft, t: Tokens) -> Element<'_, Message> {
    let dim = container(Space::with_width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.25))),
            ..container::Style::default()
        });
    let card = container(mouse_area(note_card(draft, t)).on_press(Message::PrintNop))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
    stack![mouse_area(dim).on_press(Message::NoteCancel), card,].into()
}

/// Campo de texto + Salvar/Cancelar; rótulo diz se cria ou edita.
fn note_card(draft: &NoteDraft, t: Tokens) -> Element<'_, Message> {
    let title = if draft.editing.is_some() {
        "Editar nota"
    } else {
        "Nova nota"
    };
    let can_save = !draft.text.trim().is_empty();
    let secondary = |label: &'static str, message: Message| {
        button(text(label).size(13))
            .padding(Padding::from([8, 12]))
            .style(kiri::menu_item_style(t))
            .on_press(message)
    };
    container(
        column![
            text(title).size(16),
            text_input("Escreva a nota…", &draft.text)
                .on_input(Message::NoteInput)
                .on_submit(Message::NoteSave)
                .width(Length::Fill),
            row![
                Space::with_width(Length::Fill),
                secondary("Cancelar", Message::NoteCancel),
                button(text("Salvar").size(13))
                    .padding(Padding::from([8, 12]))
                    .style(kiri::menu_item_style(t))
                    .on_press_maybe(can_save.then_some(Message::NoteSave)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(12),
    )
    .width(Length::Fixed(440.0))
    .padding(16)
    .style(kiri::menu_style(t))
    .into()
}

/// Aviso de documento assinado antes de salvar a cópia (⋯ → Salvar cópia):
/// mesmo padrão do popover de nota — fundo escurece e cancela no clique,
/// cartão engole o clique (`PrintNop` é no-op).
fn save_warning_layer(t: Tokens) -> Element<'static, Message> {
    let dim = container(Space::with_width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.25))),
            ..container::Style::default()
        });
    let card = container(mouse_area(save_warning_card(t)).on_press(Message::PrintNop))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
    stack![mouse_area(dim).on_press(Message::SaveCopyCancelled), card,].into()
}

/// Pergunta sim/não do aviso de assinatura; "Salvar mesmo assim" segue para o
/// diálogo de destino.
fn save_warning_card(t: Tokens) -> Element<'static, Message> {
    let secondary = |label: &'static str, message: Message| {
        button(text(label).size(13))
            .padding(Padding::from([8, 12]))
            .style(kiri::menu_item_style(t))
            .on_press(message)
    };
    container(
        column![
            text("Documento assinado").size(16),
            text("Salvar marcações invalida a assinatura digital. Continuar?").size(13),
            row![
                Space::with_width(Length::Fill),
                secondary("Voltar", Message::SaveCopyCancelled),
                secondary("Salvar mesmo assim", Message::SaveCopyConfirmed),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(12),
    )
    .width(Length::Fixed(440.0))
    .padding(16)
    .style(kiri::menu_style(t))
    .into()
}

fn menu_theme_button(
    t: Tokens,
    label: &'static str,
    theme: Theme,
    active: bool,
) -> Element<'static, Message> {
    control_active(
        t,
        button(text(label).size(13)).on_press(Message::SetTheme(theme)),
        active,
    )
    .width(Length::Fill)
    .into()
}

/// Progresso Kiri: 2px, fill `accent` proporcional à página visível.
fn progress(ready: &Ready, t: Tokens) -> Element<'static, Message> {
    let n = ready.page_count().max(1);
    let done = (ready.visible.index() + 1).min(n);
    // Escala fixa em 1000 partes — sem overflow de `FillPortion` em docs grandes.
    let filled = ((done * 1000) / n).clamp(1, 1000) as u16;
    let mut bar = row![container(Space::with_width(Length::Fill))
        .width(Length::FillPortion(filled))
        .height(Length::Fixed(2.0))
        .style(move |_| container::Style {
            background: Some(Background::Color(t.accent)),
            ..container::Style::default()
        }),]
    .spacing(0);
    if filled < 1000 {
        bar = bar.push(Space::new(
            Length::FillPortion(1000 - filled),
            Length::Shrink,
        ));
    }
    container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(2.0))
        .style(move |_| container::Style {
            background: Some(Background::Color(t.surface)),
            ..container::Style::default()
        })
        .into()
}

fn empty_browser(empty: &EmptyState, t: Tokens) -> Element<'_, Message> {
    let mut path_row = row![].spacing(6).align_y(Alignment::Center);
    if let Some(parent) = empty.parent() {
        path_row = path_row.push(tip(
            control(
                t,
                button(icon!(t, "chevron-left")).on_press(Message::BrowseTo(parent)),
            ),
            "Voltar",
        ));
    }
    path_row = path_row.push(text(empty.path_label()).size(14));

    let mut listing = column![].spacing(4);
    if let Some(err) = &empty.listing_error {
        listing = listing.push(text(err).size(13));
    } else if empty.listing.is_empty() {
        listing = listing.push(text("Nenhuma pasta ou PDF aqui.").size(13));
    } else {
        for entry in &empty.listing {
            listing = listing.push(entry_row(entry, t));
        }
    }

    let mut recents = column![].spacing(4);
    if empty.recents.is_empty() {
        recents = recents.push(text("Nenhum arquivo recente.").size(13));
    } else {
        for path in &empty.recents {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            recents = recents.push(
                container(
                    control(
                        t,
                        button(
                            row![icon!(t, "file-text"), text(name).size(14)]
                                .spacing(8)
                                .align_y(Alignment::Center),
                        )
                        .width(Length::Fill)
                        .on_press(Message::OpenRecent(path.clone())),
                    )
                    .width(Length::Fill),
                )
                .width(Length::Fill)
                .padding(12)
                .style(kiri::recent_card_style(t)),
            );
        }
    }

    column![
        container(
            column![
                image(image::Handle::from_bytes(
                    &include_bytes!("../../../public/tsuro-horizontal.png")[..],
                ))
                .width(Length::Fixed(200.0)),
                text("Abra um PDF ou arraste para cá")
                    .size(14)
                    .color(t.muted),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .center_x(Length::Fill)
        .padding(Padding::from([16, 0])),
        path_row,
        scrollable(listing)
            .width(Length::Fill)
            .height(Length::FillPortion(3)),
        text("Últimos arquivos").size(16),
        scrollable(recents)
            .width(Length::Fill)
            .height(Length::FillPortion(2)),
    ]
    .spacing(10)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn entry_row(entry: &FsEntry, t: Tokens) -> Element<'static, Message> {
    let message = if entry.is_dir {
        Message::BrowseTo(Some(entry.path.clone()))
    } else {
        Message::OpenRecent(entry.path.clone())
    };
    let glyph: Element<'static, Message> = if entry.is_dir {
        icon!(t, "folder").into()
    } else {
        icon!(t, "file-text").into()
    };
    control(
        t,
        button(
            row![glyph, text(entry.name.clone()).size(14)]
                .spacing(8)
                .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .on_press(message),
    )
    .width(Length::Fill)
    .into()
}

fn ready_body(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let mut panes = row![].spacing(12).height(Length::Fill);
    if ready.pages_open {
        panes = panes.push(pages_panel(ready, t));
    }
    panes = panes.push(page_pane(ready, t));
    if ready.signatures_open {
        panes = panes.push(signatures_panel(ready, t));
    }
    // Status pós-ação ("Enviado para …", "Cópia salva em …"): 1 linha no topo.
    if let Some(status) = ready.save_status.as_ref().or(ready.print_status.as_ref()) {
        column![text(status).size(13).color(t.muted), panes,]
            .spacing(8)
            .height(Length::Fill)
            .into()
    } else {
        panes.into()
    }
}

fn pages_panel(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    if ready.outline.is_some() && ready.outline_open {
        outline_tab(ready, t)
    } else {
        thumbs_tab(ready, t)
    }
}

/// Cabeçalho com abas Miniaturas | Sumário (só quando o PDF tem outline).
fn panel_tabs(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    match &ready.outline {
        None => section_title("Páginas", t),
        Some(_) => row![
            control_active(
                t,
                button(text("Miniaturas").size(12)).on_press(Message::OutlineTab(false)),
                !ready.outline_open,
            ),
            control_active(
                t,
                button(text("Sumário").size(12)).on_press(Message::OutlineTab(true)),
                ready.outline_open,
            ),
        ]
        .spacing(4)
        .into(),
    }
}

/// Aba Sumário: árvore clicável com expandir/colapsar e destaque da ativa.
fn outline_tab(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let active = ready.outline_active();
    let mut col = column![panel_tabs(ready, t)].spacing(8);
    for (path, depth, title, page, has_children) in ready.outline_rows() {
        let is_active = active.as_ref() == Some(&path);
        let fold: Element<'_, Message> = if has_children {
            let collapsed = ready.outline_collapsed.contains(&path);
            button(text(if collapsed { "▸" } else { "▾" }).size(12))
                .padding(Padding::from([4, 6]))
                .style(kiri::menu_item_style(t))
                .on_press(Message::OutlineFold(path.clone()))
                .into()
        } else {
            Space::with_width(Length::Fixed(24.0)).into()
        };
        let label = format!("{} · {}", outline_title(title), page.index() + 1);
        let entry = control_active(
            t,
            button(
                text(label)
                    .size(12)
                    .color(if is_active { t.accent } else { t.ink }),
            )
            .width(Length::Fill)
            .on_press(Message::OutlineJump(page)),
            is_active,
        );
        col = col.push(
            row![
                Space::with_width(Length::Fixed(depth as f32 * 12.0)),
                fold,
                entry,
            ]
            .spacing(2)
            .align_y(Alignment::Center),
        );
    }
    scrollable(col)
        .id(pages_scroll_id())
        .on_scroll(|viewport| Message::PagesScrolled(viewport.absolute_offset().y))
        .width(Length::Fixed(156.0))
        .height(Length::Fill)
        .into()
}

/// Título truncado em 24 caracteres para caber no painel.
fn outline_title(title: &str) -> String {
    const MAX: usize = 24;
    let end = title
        .char_indices()
        .nth(MAX)
        .map(|(i, _)| i)
        .unwrap_or(title.len());
    if end < title.len() {
        format!("{}…", &title[..end])
    } else {
        title.to_string()
    }
}
fn thumbs_tab(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    // Janela virtualizada do remoto: só monta as miniaturas visíveis.
    let window = ready.thumb_page_window();
    let start = window.first().map(|page| page.index()).unwrap_or(0);
    let end = window.last().map(|page| page.index() + 1).unwrap_or(0);
    let mut col = column![panel_tabs(ready, t)].spacing(8);
    if start > 0 {
        col = col.push(Space::with_height(Length::Fixed(start as f32 * THUMB_ROW)));
    }
    for i in start..end {
        let page = PageNo::from_index(i);
        let preview: Element<'_, Message> = match ready.thumb_surface(page) {
            Some(surface) => image(surface.image.clone())
                .width(Length::Fixed(120.0))
                .into(),
            None => container(text("…").size(13))
                .width(Length::Fixed(120.0))
                .height(Length::Fixed(150.0))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(t.elevated)),
                    border: Border {
                        color: t.line,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    text_color: Some(t.ink),
                    ..container::Style::default()
                })
                .into(),
        };
        let active = ready.visible == page;
        col = col.push(
            control_active(
                t,
                button(
                    column![
                        // Borda 2px sempre (transparente fora da ativa) — sem shift de layout.
                        container(preview)
                            .padding(0)
                            .style(move |_| container::Style {
                                border: Border {
                                    color: if active { t.accent } else { Color::TRANSPARENT },
                                    width: 2.0,
                                    radius: 4.0.into(),
                                },
                                ..container::Style::default()
                            }),
                        text(format!("{}", i + 1)).size(12)
                    ]
                    .spacing(4)
                    .align_x(Alignment::Center),
                )
                .on_press(Message::Nav(NavCmd::GoTo(page))),
                active,
            )
            .width(Length::Fill),
        );
    }
    let remaining = ready.page_count().saturating_sub(end);
    if remaining > 0 {
        col = col.push(Space::with_height(Length::Fixed(
            remaining as f32 * THUMB_ROW,
        )));
    }
    scrollable(col)
        .id(pages_scroll_id())
        .on_scroll(|viewport| Message::PagesScrolled(viewport.absolute_offset().y))
        .width(Length::Fixed(PAGES_PANEL_W))
        .height(Length::Fill)
        .into()
}

fn page_pane(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    match ready.view_mode {
        ViewMode::Single => single_pane(ready, t),
        ViewMode::Continuous => continuous_pane(ready, t),
    }
}

fn single_pane(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let cw = ready.doc_content_width();
    let page_view: Element<'_, Message> = match ready.visible_surface() {
        Some(surface) => with_marks(
            ready,
            ready.visible,
            cw,
            image(surface.image.clone()).width(Length::Fixed(cw)).into(),
        ),
        None if ready.visible_render_failed() => {
            text("Não foi possível renderizar esta página.").into()
        }
        None => text("Renderizando página…").into(),
    };

    scrollable(
        container(
            container(page_view)
                .width(Length::Fill)
                .center_x(Length::Fill)
                .padding(0)
                .style(kiri::page_frame(&t)),
        )
        .width(Length::Fill)
        .center_x(Length::Fill)
        .padding(Padding {
            top: DOC_PAD_TOP,
            right: DOC_PAD_X,
            bottom: DOC_PAD_BOTTOM,
            left: DOC_PAD_X,
        })
        .style(move |_| container::Style {
            background: Some(Background::Color(t.surface)),
            ..container::Style::default()
        }),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Rolagem contínua: coluna de células com a mesma estrutura da página única;
/// fora da janela, placeholders de altura exata (sem montar bitmaps).
fn continuous_pane(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let cw = ready.doc_content_width();
    let total = ready.page_count();
    let (start, end) = ready.doc_window();
    let mut col = column![].spacing(DOC_GAP).width(Length::Fill);
    if start > 0 {
        // Offset acumulado menos um gap (o spacing da coluna já conta um).
        let h = (ready.page_offset(PageNo::from_index(start)) - DOC_GAP).max(0.0);
        col = col.push(Space::with_height(Length::Fixed(h)));
    }
    for i in start..end {
        col = col.push(doc_cell(ready, PageNo::from_index(i), cw, t));
    }
    if end < total {
        let h = (ready.doc_total_height() - ready.page_offset(PageNo::from_index(end)) - DOC_GAP)
            .max(0.0);
        col = col.push(Space::with_height(Length::Fixed(h)));
    }
    scrollable(col)
        .id(doc_scroll_id())
        .on_scroll(|viewport| Message::DocScrolled(viewport.absolute_offset().y))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn doc_cell(ready: &Ready, page: PageNo, cw: f32, t: Tokens) -> Element<'_, Message> {
    let inner: Element<'_, Message> = match ready.page_surface(page) {
        Some(surface) => with_marks(
            ready,
            page,
            cw,
            image(surface.image.clone()).width(Length::Fixed(cw)).into(),
        ),
        None => {
            let h = (ready.doc_cell_height(page, cw) - DOC_PAD_TOP - DOC_PAD_BOTTOM).max(1.0);
            container(text("Renderizando página…").size(13).color(t.muted))
                .width(Length::Fill)
                .height(Length::Fixed(h))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        }
    };
    container(
        container(inner)
            .width(Length::Fill)
            .center_x(Length::Fill)
            .padding(0)
            .style(kiri::page_frame(&t)),
    )
    .width(Length::Fill)
    .center_x(Length::Fill)
    .padding(Padding {
        top: DOC_PAD_TOP,
        right: DOC_PAD_X,
        bottom: DOC_PAD_BOTTOM,
        left: DOC_PAD_X,
    })
    .style(move |_| container::Style {
        background: Some(Background::Color(t.surface)),
        ..container::Style::default()
    })
    .into()
}

/// Retângulo desenhável (px CSS, espaço exibido); `kind: None` = seleção ativa.
/// `marker: true` = quadrado compacto de nota (não segue o traço do kind).
struct DrawMark {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    kind: Option<AnnotKind>,
    marker: bool,
}

/// Camada transparente sobre a folha (issue #30): desenha marcações/seleção
/// e traduz o arrasto em PointerDown/Move/Up (a seleção não tinha emissor).
struct MarkLayer {
    page: PageNo,
    media: MediaBox,
    rotation: u8,
    size: Size,
    marks: Vec<DrawMark>,
}

#[derive(Default)]
struct MarkDrag {
    pressing: bool,
}

impl Program<Message> for MarkLayer {
    type State = MarkDrag;

    fn update(
        &self,
        state: &mut Self::State,
        event: CanvasEvent,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (CanvasStatus, Option<Message>) {
        let page_pt = |p: Point| {
            page_pt_at(
                [p.x, p.y],
                self.media,
                self.rotation,
                self.size.width,
                self.size.height,
            )
        };
        match event {
            CanvasEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                match cursor.position_in(bounds) {
                    Some(at) => {
                        state.pressing = true;
                        (
                            CanvasStatus::Captured,
                            Some(Message::PointerDown {
                                page: self.page,
                                page_pt: page_pt(at),
                            }),
                        )
                    }
                    None => (CanvasStatus::Ignored, None),
                }
            }
            CanvasEvent::Mouse(mouse::Event::CursorMoved { .. }) => {
                if !state.pressing {
                    return (CanvasStatus::Ignored, None);
                }
                // Fora da folha, fixa na borda (o arrasto continua valendo).
                let at = cursor.position_in(bounds).unwrap_or_else(|| {
                    let p = cursor.position().unwrap_or(Point::new(0.0, 0.0));
                    Point::new(
                        (p.x - bounds.x).clamp(0.0, bounds.width),
                        (p.y - bounds.y).clamp(0.0, bounds.height),
                    )
                });
                (
                    CanvasStatus::Ignored,
                    Some(Message::PointerMove {
                        page: self.page,
                        page_pt: page_pt(at),
                    }),
                )
            }
            CanvasEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if !state.pressing {
                    return (CanvasStatus::Ignored, None);
                }
                state.pressing = false;
                match cursor.position_in(bounds) {
                    Some(at) => (
                        CanvasStatus::Ignored,
                        Some(Message::PointerUp {
                            page: self.page,
                            page_pt: page_pt(at),
                        }),
                    ),
                    None => (CanvasStatus::Ignored, None),
                }
            }
            _ => (CanvasStatus::Ignored, None),
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, self.size);
        for m in &self.marks {
            let rect = Path::rectangle(Point::new(m.x, m.y), Size::new(m.w, m.h));
            // Nota: amarelo fixo — a folha é sempre branca, o tema não se aplica.
            if m.marker {
                frame.fill(&rect, Color::from_rgb(0.99, 0.80, 0.20));
                frame.stroke(
                    &rect,
                    Stroke {
                        style: Style::Solid(Color::from_rgb(0.42, 0.30, 0.02)),
                        width: 1.0,
                        ..Stroke::default()
                    },
                );
                continue;
            }
            match m.kind {
                None => frame.fill(&rect, Color::from_rgba(0.25, 0.45, 1.0, 0.30)),
                Some(AnnotKind::Highlight) => {
                    frame.fill(&rect, Color::from_rgba(1.0, 0.85, 0.25, 0.45))
                }
                Some(AnnotKind::Underline) => frame.stroke(
                    &Path::line(
                        Point::new(m.x, m.y + m.h - 1.0),
                        Point::new(m.x + m.w, m.y + m.h - 1.0),
                    ),
                    Stroke {
                        style: Style::Solid(Color::from_rgb(0.1, 0.35, 0.9)),
                        width: 2.0,
                        ..Stroke::default()
                    },
                ),
                // Nota: sublinhado sutil só para ancorar o trecho na folha.
                Some(AnnotKind::Note) => frame.stroke(
                    &Path::line(
                        Point::new(m.x, m.y + m.h - 1.0),
                        Point::new(m.x + m.w, m.y + m.h - 1.0),
                    ),
                    Stroke {
                        style: Style::Solid(Color::from_rgba(0.85, 0.62, 0.05, 0.75)),
                        width: 1.5,
                        ..Stroke::default()
                    },
                ),
                Some(AnnotKind::Strikeout) => frame.stroke(
                    &Path::line(
                        Point::new(m.x, m.y + m.h * 0.5),
                        Point::new(m.x + m.w, m.y + m.h * 0.5),
                    ),
                    Stroke {
                        style: Style::Solid(Color::from_rgb(0.85, 0.15, 0.2)),
                        width: 2.0,
                        ..Stroke::default()
                    },
                ),
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        mouse::Interaction::Text
    }
}

/// Folha com overlay: imagem em tamanho fixo + canvas transparente da mesma
/// medida (view e canvas dividem `cw`/`ch`, então o mapeamento alinha).
fn with_marks<'a>(
    ready: &Ready,
    page: PageNo,
    cw: f32,
    sheet: Element<'a, Message>,
) -> Element<'a, Message> {
    let media = ready.media(page);
    let rotated = ready.rotated_media(page);
    let ch = cw * rotated.height.max(1.0) / rotated.width.max(1.0);
    let size = Size::new(cw, ch);
    let mut marks = Vec::new();
    if let Some((sel_page, quads)) = ready.selection_quads() {
        if sel_page == page {
            for quad in &quads {
                let [x, y, w, h] = display_rect(*quad, media, ready.view_rotation, cw, ch);
                marks.push(DrawMark {
                    x,
                    y,
                    w,
                    h,
                    kind: None,
                    marker: false,
                });
            }
        }
    }
    for a in ready.annotations.iter().filter(|a| a.page == page) {
        for (i, quad) in a.quads.iter().enumerate() {
            let [x, y, w, h] = display_rect(*quad, media, ready.view_rotation, cw, ch);
            marks.push(DrawMark {
                x,
                y,
                w,
                h,
                kind: Some(a.kind),
                marker: false,
            });
            // Nota: marcador compacto na origem do primeiro quad.
            if a.kind == AnnotKind::Note && i == 0 {
                let side = h.clamp(6.0, 14.0);
                marks.push(DrawMark {
                    x,
                    y,
                    w: side,
                    h: side,
                    kind: Some(AnnotKind::Note),
                    marker: true,
                });
            }
        }
    }
    let layer = MarkLayer {
        page,
        media,
        rotation: ready.view_rotation,
        size,
        marks,
    };
    stack![
        sheet,
        Canvas::new(layer)
            .width(Length::Fixed(cw))
            .height(Length::Fixed(ch)),
    ]
    .into()
}

fn signatures_panel(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let mut col = column![section_title("Assinaturas", t)].spacing(8);
    if ready.signatures.signatures.is_empty() {
        col = col.push(
            text("Nenhuma assinatura neste arquivo.")
                .size(13)
                .color(t.muted),
        );
    } else {
        for sig in &ready.signatures.signatures {
            let name = sig
                .signer_name
                .as_deref()
                .or(sig
                    .certificate
                    .as_ref()
                    .and_then(|c| c.common_name.as_deref()))
                .unwrap_or("Assinante");
            let dot = kiri::status_color(t, sig.status);
            col = col.push(
                container(
                    column![
                        row![status_dot(dot), text(name).size(14).color(t.ink),]
                            .spacing(8)
                            .align_y(Alignment::Center),
                        text(status_label(sig.status)).size(13).color(t.ink),
                        text(&sig.status_detail).size(12).color(t.muted),
                    ]
                    .spacing(4),
                )
                .width(Length::Fill)
                .padding(12)
                .style(move |_| container::Style {
                    background: Some(Background::Color(t.elevated)),
                    border: Border {
                        color: t.line,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    text_color: Some(t.ink),
                    ..container::Style::default()
                }),
            );
        }
    }
    container(scrollable(col).height(Length::Fill))
        .width(Length::Fixed(SIG_PANEL_W))
        .height(Length::Fill)
        .padding(Padding::from([4, 0]))
        .style(move |_| container::Style {
            background: Some(Background::Color(t.bg)),
            ..container::Style::default()
        })
        .into()
}

/// Título de seção de painel: 12px, maiúsculas, `muted`.
fn section_title(label: &'static str, t: Tokens) -> Element<'static, Message> {
    text(label.to_uppercase()).size(12).color(t.muted).into()
}

/// Dot 8px de status (cartões de assinatura).
fn status_dot(color: Color) -> Element<'static, Message> {
    container(Space::with_width(Length::Fixed(8.0)))
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |_| container::Style {
            background: Some(Background::Color(color)),
            border: Border {
                radius: 99.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

fn status_label(status: SignatureStatus) -> &'static str {
    match status {
        SignatureStatus::Valid => "Válida",
        SignatureStatus::IntactButUntrusted => "Íntegra (sem confiança pública)",
        SignatureStatus::DocumentModified => "Documento alterado",
        SignatureStatus::Invalid => "Inválida",
        SignatureStatus::Unsupported => "Não suportada",
        SignatureStatus::CertificateExpired => "Certificado expirado",
        SignatureStatus::CertificateNotYetValid => "Certificado ainda não válido",
    }
}

#[cfg(test)]
mod tests {
    use super::outline_title;

    #[test]
    fn outline_title_truncates_long_labels() {
        assert_eq!(outline_title("Curto"), "Curto");
        assert_eq!(outline_title(""), "");
        let long = "Cláusula de rescisão contratual e multa por descumprimento";
        let short = outline_title(long);
        assert!(short.ends_with('…'));
        assert_eq!(short.chars().count(), 25);
        // Exatos 24 caracteres passam intactos.
        let exact: String = "a".repeat(24);
        assert_eq!(outline_title(&exact), exact);
    }
}
