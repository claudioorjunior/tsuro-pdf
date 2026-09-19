use iced::advanced::widget::operation::{Focusable, Operation};
use iced::advanced::widget::{operate, Id as WidgetId};
use iced::widget::canvas::event::{Event as CanvasEvent, Status as CanvasStatus};
use iced::widget::canvas::{Frame, Geometry, LineDash, Path, Program, Stroke, Style};
use iced::widget::{
    button, column, container, image, mouse_area, pick_list, row, scrollable, stack, text,
    text_editor, text_input, tooltip, Canvas, Space,
};
use iced::{mouse, Alignment, Background, Border, Color, Element, Length, Padding, Point, Task};
use iced::{Rectangle, Size};
use tsuro_sign::SignatureStatus;

use crate::browse::{EmptyState, FsEntry};
use crate::kiri::{self, Theme, Tokens};
use crate::page::{MediaBox, PageNo};
use crate::print::{PrintOrientation, MAX_COPIES};
use crate::session::{
    display_pt, display_rect, marker_side, page_pt_at, AnnotKind, Message, NavCmd, NoteDraft,
    OpenSource, PrintDialog, RangeMode, Ready, Session, Tabs, ViewMode, Zoom, ZoomFactor, DOC_GAP,
    DOC_PAD_BOTTOM, DOC_PAD_TOP, DOC_PAD_X, PAGES_PANEL_W, SIG_PANEL_W, THUMB_ROW,
};

/// Altura do chrome Kiri: toolbar 36px + progresso 2px + respiro.
pub const CHROME_HEIGHT: f32 = 46.0;

/// Altura da faixa de abas (issue #40), reservada só com 2+ documentos.
pub const TAB_STRIP_HEIGHT: f32 = 30.0;

/// Papel do post-it (issue #30): amarelo fixo, como o marcador nota do
/// canvas — papel não segue o tema e a tinta escura lê nos dois.
const STICKY: Color = Color::from_rgb(0.99, 0.80, 0.20);
/// Tinta sobre o papel (marcador, rótulo, texto e botões do post-it).
const STICKY_INK: Color = Color::from_rgb(0.42, 0.30, 0.02);
/// Borda do papel (um tom abaixo do fundo).
const STICKY_LINE: Color = Color::from_rgb(0.80, 0.62, 0.10);
/// Post-it: largura, altura do cartão (posição presa por ela) e altura do
/// campo de texto. A altura do cartão é a do conteúdo montado abaixo
/// (rótulo + editor + rodapé + paddings), só para prender a posição.
const POSTIT_W: f32 = 320.0;
const POSTIT_H: f32 = 210.0;
const POSTIT_EDITOR_H: f32 = 112.0;
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
    if let Session::Ready(tabs) = session {
        col = col.push(progress(tabs, t));
        // Faixa de abas (issue #40): com um documento só a janela é a de
        // sempre — sem faixa nenhuma.
        if tabs.len() > 1 {
            col = col.push(tab_strip(tabs, t));
        }
    }
    col = col.push(body);
    let main: Element<'_, Message> = container(
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
    })
    .into();
    // HUD flutuante (Stitch): pílula bottom-center sobre o canvas, sob os
    // modais — que escurecem por cima e capturam tudo.
    let main = match session {
        Session::Ready(ready) => stack![main, hud(ready, t)].into(),
        _ => main,
    };
    match session {
        // Modal de impressão captura tudo; menu ⋯ nunca abre junto (fecha ao abrir).
        Session::Ready(ready) if ready.print_dialog.is_some() => {
            let dialog = ready.print_dialog.as_ref().expect("checked above");
            stack![main, print_layer(ready, dialog, t)].into()
        }
        // Post-it (issue #30): editor ancorado no trecho da nota. Sem modal
        // centrado — só o fundo escurece e cancela no clique (como o de
        // impressão), que também captura os cliques de fora.
        Session::Ready(ready) if ready.note_draft.is_some() => {
            stack![main, note_layer(ready, t)].into()
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

/// Botão-ícone da pílula do HUD: Ori + tooltip, no tamanho dos segmentos.
fn hud_icon(
    file: &str,
    label: &'static str,
    t: Tokens,
    message: Message,
) -> Element<'static, Message> {
    tip(
        control_seg(t, button(kiri::ori_icon(file, 16.0))).on_press(message),
        label,
    )
}

/// HUD flutuante (Stitch §Floating Action Pill): pílula 36px centrada a 24px do
/// rodapé, sobre o canvas. Só os botões capturam clique — o resto atravessa.
fn hud(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let n = ready.page_count().max(1);
    let page = ready.visible.index() + 1;
    let current = ready.zoom_step_factor();
    let out = Zoom::Manual(ZoomFactor::new(current / 1.1));
    let into = Zoom::Manual(ZoomFactor::new(current * 1.1));
    let mono = |s: String, color: Color| text(s).size(12).font(iced::Font::MONOSPACE).color(color);
    let mut pill = row![
        hud_icon(
            "chevron-left",
            "Página anterior",
            t,
            Message::Nav(NavCmd::Previous)
        ),
        mono(page.to_string(), t.ink),
        mono("/".to_string(), t.muted),
        mono(n.to_string(), t.muted),
        hud_icon(
            "chevron-right",
            "Próxima página",
            t,
            Message::Nav(NavCmd::Next)
        ),
        kiri::vsep(t),
        hud_icon(
            "fit-width",
            "Ajustar à largura",
            t,
            Message::SetZoom(Zoom::Width)
        ),
        hud_icon("minus", "Diminuir zoom", t, Message::SetZoom(out)),
        mono(format!("{}%", (current * 100.0).round() as i32), t.ink),
        hud_icon("plus", "Aumentar zoom", t, Message::SetZoom(into)),
    ]
    .spacing(2)
    .align_y(Alignment::Center);
    // Modo Anotação: só com seleção viva ou marcações na sessão.
    if ready.selection_plain_text().is_some() || !ready.annotations.is_empty() {
        pill = pill.push(kiri::vsep(t)).push(
            row![
                container(Space::with_width(Length::Fixed(7.0)))
                    .width(Length::Fixed(7.0))
                    .height(Length::Fixed(7.0))
                    .style(move |_| container::Style {
                        background: Some(Background::Color(t.accent)),
                        border: Border {
                            radius: 99.0.into(),
                            ..Border::default()
                        },
                        ..container::Style::default()
                    }),
                text("Modo Anotação").size(11).color(t.accent),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    container(
        container(pill)
            .height(Length::Fixed(28.0))
            .align_y(Alignment::Center)
            .padding(Padding::from([4, 6]))
            .style(kiri::hud_style(t)),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::End)
    .padding(Padding {
        top: 0.0,
        right: 0.0,
        bottom: 24.0,
        left: 0.0,
    })
    .into()
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
        control(t, button(kiri::ori!("folder")).on_press(Message::PickFile)),
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

/// Nome do arquivo com elipse no meio (`Contrato_Locacao_..._2024.pdf`).
fn middle_truncate(name: &str, max: usize) -> String {
    let count = name.chars().count();
    if count <= max {
        return name.to_string();
    }
    // A elipse ocupa 1; o resto divide cabeça e cauda.
    let head = (max - 1) / 2;
    let tail = max - 1 - head;
    let head_end = name.char_indices().nth(head).map_or(name.len(), |(i, _)| i);
    let tail_start = name
        .char_indices()
        .nth(count - tail)
        .map_or(name.len(), |(i, _)| i);
    format!("{}…{}", &name[..head_end], &name[tail_start..])
}

/// Barra única Kiri (`Session::Ready`): abrir │ pílula do documento │ busca e
/// página │ marcar │ zoom │ painéis │ ⋯. Uma linha só (`CHROME_HEIGHT`).
fn topbar(session: &Session, t: Tokens) -> Element<'_, Message> {
    if let Session::Ready(ready) = session {
        let n = ready.page_count().max(1);
        // Pílula do documento: dot (marcas da sessão ainda não salvas) + nome do
        // arquivo com elipse no meio. Nada de tamanho/data: só o que o estado tem.
        let name = ready
            .source
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut doc_row = row![].spacing(6).align_y(Alignment::Center);
        if !ready.annotations.is_empty() {
            doc_row = doc_row.push(
                container(Space::with_width(Length::Fixed(6.0)))
                    .width(Length::Fixed(6.0))
                    .height(Length::Fixed(6.0))
                    .style(kiri::bar_dot_style(t)),
            );
        }
        let doc_pill = tip(
            container(doc_row.push(text(middle_truncate(&name, 24)).size(13).color(t.ink)))
                .padding(Padding {
                    top: 4.0,
                    right: 8.0,
                    bottom: 4.0,
                    left: 8.0,
                })
                .style(kiri::bar_doc_style(t)),
            "Documento aberto",
        );
        let pill = container(
            row![
                kiri::ori_small!("search"),
                text_input("Buscar no documento...", ready.search.query())
                    .on_input(Message::SearchChanged)
                    .style(kiri::bar_input_style(t))
                    .padding([2, 4])
                    .size(12)
                    .width(Length::Fixed(180.0)),
                kiri::vsep(t),
                row![
                    tip(
                        text_input("Página", ready.page_input())
                            .on_input(Message::PageInput)
                            .on_submit(Message::PageSubmit)
                            .style(kiri::bar_input_style(t))
                            .font(iced::Font::MONOSPACE)
                            .size(12)
                            .padding([2, 4])
                            .width(Length::Fixed(30.0)),
                        "Ir para página (Enter confirma)",
                    ),
                    text(format!("/{n}"))
                        .size(12)
                        .font(iced::Font::MONOSPACE)
                        .color(t.muted),
                ]
                .spacing(2)
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
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding(Padding {
            top: 2.0,
            right: 6.0,
            bottom: 2.0,
            left: 10.0,
        })
        .style(kiri::pill_style(t))
        .max_width(520.0);
        let current = ready.zoom_step_factor();
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
                    container(
                        text(format!("{}%", (current * 100.0).round() as i32))
                            .size(12)
                            .font(iced::Font::MONOSPACE)
                            .color(t.muted),
                    )
                    .padding(Padding::from([0, 4])),
                    "Zoom atual",
                ),
                tip(
                    control_seg(
                        t,
                        button(kiri::ori!("plus")).on_press(Message::SetZoom(into))
                    ),
                    "Aumentar zoom"
                ),
                tip(
                    control_seg(
                        t,
                        button(kiri::ori!("fit-width")).on_press(Message::SetZoom(Zoom::Width))
                    ),
                    "Ajustar à largura"
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

        // Barra de anotar na cara (seleção viva): ⋯ é fuga, não casa. Os atalhos
        // H/U/S/N já vivem em `keyboard_message`; aqui só o rótulo da tecla.
        let mut bar = row![
            home_button(t),
            open_button(t),
            kiri::vsep(t),
            doc_pill,
            Space::with_width(Length::Fill),
            pill,
            Space::with_width(Length::Fill),
        ]
        .spacing(4)
        .align_y(Alignment::Center);
        if ready.selection_plain_text().is_some() {
            let hint = |key: &'static str| {
                text(key)
                    .size(10)
                    .font(iced::Font::MONOSPACE)
                    .color(t.muted)
            };
            // Rótulo só com janela larga (Stitch: `hidden xl:inline`, 1280px);
            // estreita fica ícone + tecla, que é o que cabe na mesma linha.
            let wide = ready.viewport().width >= 1280.0;
            let tool = |icon: Element<'static, Message>, label: &'static str, key: &'static str| {
                let mut content = row![icon].spacing(4).align_y(Alignment::Center);
                if wide {
                    content = content.push(text(label).size(12));
                }
                content.push(hint(key))
            };
            let mark_seg = container(
                row![
                    tip(
                        control_seg(
                            t,
                            button(tool(kiri::ori!("highlighter"), "Destacar", "H"))
                                .on_press(Message::Annotate(AnnotKind::Highlight))
                        ),
                        "Destacar (H)"
                    ),
                    tip(
                        control_seg(
                            t,
                            button(tool(kiri::ori!("underline"), "Sublinhar", "U"))
                                .on_press(Message::Annotate(AnnotKind::Underline))
                        ),
                        "Sublinhar (U)"
                    ),
                    tip(
                        control_seg(
                            t,
                            button(tool(kiri::ori!("strike"), "Riscar", "S"))
                                .on_press(Message::Annotate(AnnotKind::Strikeout))
                        ),
                        "Riscar (S)"
                    ),
                    tip(
                        control_seg(
                            t,
                            button(tool(kiri::ori!("note"), "Nota", "N"))
                                .on_press(Message::Annotate(AnnotKind::Note))
                        ),
                        "Nota (N)"
                    ),
                ]
                .spacing(0)
                .align_y(Alignment::Center),
            )
            .padding(2)
            .style(kiri::bar_mark_style(t));
            bar = bar.push(mark_seg).push(kiri::vsep(t));
        }
        bar = bar.push(zoom_seg).push(kiri::vsep(t)).push(right);

        return toolbar_frame(t, bar.into());
    }

    // Tela inicial não precisa de home; erro/carregando usam para voltar.
    let mut items: Vec<Element<'_, Message>> = Vec::new();
    if !matches!(session, Session::Empty(_)) {
        items.push(home_button(t));
    }
    items.push(open_button(t));
    toolbar_frame(t, row(items).spacing(4).align_y(Alignment::Center).into())
}

/// Camada do menu ⋯: fundo fecha ao clicar, menu no canto.
fn overflow_layer(tabs: &Tabs, t: Tokens) -> Element<'_, Message> {
    let dim = container(Space::with_width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.25))),
            ..container::Style::default()
        });
    // Com 2+ abas a faixa soma TAB_STRIP_HEIGHT à topbar (44px).
    let top = if tabs.len() > 1 {
        44.0 + TAB_STRIP_HEIGHT
    } else {
        44.0
    };
    let card = container(overflow_menu(tabs, t))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::End)
        .align_y(Alignment::Start)
        .padding(Padding {
            top,
            right: 8.0,
            bottom: 0.0,
            left: 0.0,
        });
    stack![mouse_area(dim).on_press(Message::ToggleOverflow), card].into()
}

/// Menu ⋯ (PR 4): zoom, girar, imprimir, histórico, modo, copiar,
/// desfazer/refazer, salvar, fechar, aparência.
/// Ícones Ori nas linhas acionáveis: fit-page, rotate, print,
/// chevron-left, chevron-right, page-single, continuous, copy, undo,
/// redo, save, x. O modo vigente leva destaque `accent` (sem ●/○ —
/// faltam na fonte e viram `?`). Headers e Escuro/Claro sem ícone.
fn overflow_menu(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let mut items = column![].spacing(2).width(Length::Fill);
    items = items.push(menu_item(
        t,
        "fit-page",
        "Ajustar página inteira",
        Message::SetZoom(Zoom::Page),
        false,
    ));
    items = items.push(menu_item(
        t,
        "rotate",
        "Girar vista (90°)",
        Message::RotateView,
        false,
    ));
    items = items.push(menu_item(
        t,
        "print",
        "Imprimir",
        Message::OpenPrintDialog,
        false,
    ));
    if ready.can_history_back() {
        items = items.push(menu_item(
            t,
            "chevron-left",
            "Voltar",
            Message::HistoryBack,
            false,
        ));
    }
    if ready.can_history_forward() {
        items = items.push(menu_item(
            t,
            "chevron-right",
            "Avançar",
            Message::HistoryForward,
            false,
        ));
    }
    items = items.push(text("Modo de página").size(12).color(t.muted));
    let single = ready.view_mode == ViewMode::Single;
    items = items.push(menu_item(
        t,
        "page-single",
        "Página única",
        Message::SetViewMode(ViewMode::Single),
        single,
    ));
    items = items.push(menu_item(
        t,
        "continuous",
        "Rolagem contínua",
        Message::SetViewMode(ViewMode::Continuous),
        !single,
    ));
    if ready.selection_plain_text().is_some() {
        items = items.push(menu_item(
            t,
            "copy",
            "Copiar seleção",
            Message::CopySelection,
            false,
        ));
    }
    if ready.can_annot_undo() {
        items = items.push(menu_item(
            t,
            "undo",
            "Desfazer marcação",
            Message::AnnotUndo,
            false,
        ));
    }
    if ready.can_annot_redo() {
        items = items.push(menu_item(
            t,
            "redo",
            "Refazer marcação",
            Message::AnnotRedo,
            false,
        ));
    }
    if !ready.annotations.is_empty() {
        items = items.push(menu_item(
            t,
            "save",
            "Salvar cópia com marcações…",
            Message::SaveCopyRequested,
            false,
        ));
    }
    items = items.push(menu_item(t, "x", "Fechar documento", Message::Close, false));
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

/// Linha do menu ⋯: ícone Ori + rótulo; `active` pinta o modo vigente
/// (`accent_bg` + texto `accent`, via `panel_seg_style`).
fn menu_item(
    t: Tokens,
    icon: &str,
    label: &'static str,
    message: Message,
    active: bool,
) -> Element<'static, Message> {
    button(
        row![kiri::ori_icon(icon, 16.0), text(label).size(13)]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(Padding::from([8, 10]))
    .style(kiri::panel_seg_style(t, active))
    .on_press(message)
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
            .style(kiri::hud_ghost_style(t))
            .on_press_maybe((!busy).then_some(message))
    };
    row![
        Space::with_width(Length::Fill),
        secondary("Abrir PDF", Message::PrintOpenPdf),
        secondary("Cancelar", Message::ClosePrintDialog),
        button(text(label).size(13))
            .padding(Padding::from([8, 12]))
            .style(kiri::hud_primary_style(t))
            .on_press_maybe((!busy).then_some(Message::PrintSubmit)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// Post-it (issue #30): o editor abre ancorado no trecho da nota, não no
/// centro da janela — o rascunho guarda o canto (origem da folha no clique +
/// rolagem) e a vista o desenha ali, preso à janela. Mesmo padrão do modal de
/// impressão: o fundo escurece e cancela no clique, o cartão engole o clique
/// (`PrintNop` é no-op) e o editor dentro dele recebe os eventos primeiro.
fn note_layer(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let Some(draft) = ready.note_draft.as_ref() else {
        return Space::with_width(Length::Fill).into();
    };
    let [x, y] = ready.postit_pos([POSTIT_W, POSTIT_H]);
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
        .align_x(Alignment::Start)
        .align_y(Alignment::Start)
        .padding(Padding {
            top: y,
            right: 0.0,
            bottom: 0.0,
            left: x,
        });
    stack![mouse_area(dim).on_press(Message::NoteCancel), card,].into()
}

/// Cartão do post-it: rótulo caps + editor multilinha + rodapé com o vermelho
/// `remover nota` (só em edição), `Cancelar` ghost e `Salvar` primária. Texto
/// escuro fixo: o papel é amarelo em qualquer tema.
fn note_card(draft: &NoteDraft, t: Tokens) -> Element<'_, Message> {
    let editing = draft.editing.is_some();
    let can_save = !draft.content.text().trim().is_empty();
    let mut footer = row![].spacing(8).align_y(Alignment::Center);
    if editing {
        footer = footer.push(
            button(text("remover nota").size(13))
                .padding(Padding::from([8, 12]))
                .style(sticky_button_style(t.danger))
                .on_press(Message::NoteDelete),
        );
    }
    container(
        column![
            text(if editing { "EDITAR NOTA" } else { "NOVA NOTA" })
                .size(10)
                .color(STICKY_INK),
            container(Space::with_height(Length::Fixed(1.0)))
                .width(Length::Fill)
                .style(|_| container::Style {
                    background: Some(Background::Color(STICKY_LINE)),
                    ..container::Style::default()
                }),
            text_editor(&draft.content)
                .placeholder("Escreva a nota…")
                .on_action(Message::NoteEdit)
                .padding(6)
                .size(13)
                .height(Length::Fixed(POSTIT_EDITOR_H))
                .style(sticky_editor_style),
            footer
                .push(Space::with_width(Length::Fill))
                .push(
                    button(text("Cancelar").size(13))
                        .padding(Padding::from([8, 12]))
                        .style(sticky_button_style(STICKY_INK))
                        .on_press(Message::NoteCancel),
                )
                .push(
                    button(text("Salvar").size(13))
                        .padding(Padding::from([8, 12]))
                        .style(kiri::hud_primary_style(t))
                        .on_press_maybe(can_save.then_some(Message::NoteSave)),
                ),
        ]
        .spacing(8),
    )
    .width(Length::Fixed(POSTIT_W))
    .padding(12)
    .style(sticky_style)
    .into()
}

/// Papel do post-it: fundo amarelo, borda discreta e sombra de papel solto
/// (o cartão flutua sobre a folha, então precisa descolar dela).
fn sticky_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(STICKY)),
        border: Border {
            color: STICKY_LINE,
            width: 1.0,
            radius: 3.0.into(),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 16.0,
        },
        text_color: Some(STICKY_INK),
        ..container::Style::default()
    }
}

/// Botão de texto sobre o papel (ghost): sem fundo, tinta da cor pedida —
/// `t.danger` no `remover nota`, a tinta do papel no `Cancelar`.
fn sticky_button_style(color: Color) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let interactive = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if interactive {
                Color::from_rgba(0.0, 0.0, 0.0, 0.10)
            } else {
                Color::TRANSPARENT
            })),
            text_color: color,
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            shadow: iced::Shadow::default(),
        }
    }
}

/// Editor sobre o papel: fundo do próprio papel (sem caixa branca), tinta
/// escura e cursor/seleção legíveis no amarelo.
fn sticky_editor_style(_theme: &iced::Theme, _status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: Background::Color(STICKY),
        border: Border::default(),
        icon: STICKY_INK,
        placeholder: Color::from_rgba(STICKY_INK.r, STICKY_INK.g, STICKY_INK.b, 0.55),
        value: STICKY_INK,
        selection: Color::from_rgba(0.42, 0.30, 0.02, 0.25),
    }
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
    container(
        column![
            text("Documento assinado").size(16),
            text("Salvar marcações invalida a assinatura digital. Continuar?").size(13),
            row![
                Space::with_width(Length::Fill),
                button(text("Voltar").size(13))
                    .padding(Padding::from([8, 12]))
                    .style(kiri::hud_ghost_style(t))
                    .on_press(Message::SaveCopyCancelled),
                button(text("Salvar mesmo assim").size(13))
                    .padding(Padding::from([8, 12]))
                    .style(kiri::hud_primary_style(t))
                    .on_press(Message::SaveCopyConfirmed),
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

/// Moldura tracejada da dropzone (Stitch). `Border` do iced não tem dash,
/// então o retângulo vem do canvas — 1px `line`, traço 6/4.
struct EmptyDash {
    color: Color,
}

impl Program<Message> for EmptyDash {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // Meio pixel para dentro: o traço de 1px cai na linha física.
        let rect = Path::rectangle(
            Point::new(0.5, 0.5),
            Size::new(bounds.width - 1.0, bounds.height - 1.0),
        );
        frame.stroke(
            &rect,
            Stroke {
                style: Style::Solid(self.color),
                width: 1.0,
                line_dash: LineDash {
                    segments: &[6.0, 4.0],
                    offset: 0,
                },
                ..Stroke::default()
            },
        );
        vec![frame.into_geometry()]
    }
}

/// Estado vazio (Stitch): dropzone tracejada, grid de "Documentos recentes"
/// e, abaixo, o navegador de pastas de sempre (o Stitch não o mostra, mas o
/// comportamento fica — só discreto, sem título próprio).
fn empty_browser(empty: &EmptyState, t: Tokens) -> Element<'_, Message> {
    let mut path_row = row![].spacing(6).align_y(Alignment::Center);
    if let Some(parent) = empty.parent() {
        path_row = path_row.push(tip(
            control(
                t,
                button(kiri::ori!("chevron-left")).on_press(Message::BrowseTo(parent)),
            ),
            "Voltar",
        ));
    }
    path_row = path_row.push(text(empty.path_label()).size(14).color(t.muted));

    let mut listing = column![].spacing(4);
    if let Some(err) = &empty.listing_error {
        listing = listing.push(text(err).size(13).color(t.muted));
    } else if empty.listing.is_empty() {
        listing = listing.push(text("Nenhuma pasta ou PDF aqui.").size(13).color(t.muted));
    } else {
        for entry in &empty.listing {
            listing = listing.push(entry_row(entry, t));
        }
    }

    let mut recents = column![].spacing(10).width(Length::Fill);
    if empty.recents.is_empty() {
        recents = recents.push(text("Nenhum arquivo recente.").size(13).color(t.muted));
    } else {
        for chunk in empty.recents.chunks(3) {
            let mut line = row![].spacing(10).width(Length::Fill);
            for path in chunk {
                line = line.push(empty_card(path, t));
            }
            // Espaços fecham a linha: 3 colunas de largura igual.
            for _ in chunk.len()..3 {
                line = line.push(Space::with_width(Length::Fill));
            }
            recents = recents.push(line);
        }
    }

    let header = row![
        section_title("Documentos recentes", t),
        container(
            text(empty.recents.len().to_string())
                .size(10)
                .color(t.muted)
        )
        .padding(Padding::from([1, 6]))
        .style(kiri::empty_badge_style(t)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let drop_h = 248.0;
    let dropzone = container(
        container(stack![
            Canvas::new(EmptyDash { color: t.line })
                .width(Length::Fill)
                .height(Length::Fixed(drop_h)),
            container(
                column![
                    container(
                        image(image::Handle::from_bytes(
                            &include_bytes!("../../../public/tsuro-horizontal-pdf.png")[..],
                        ))
                        .width(Length::Fixed(200.0)),
                    )
                    .padding(8)
                    .style(kiri::logo_stage_style()),
                    text("Nenhum documento aberto").size(18),
                    text("Arraste e solte um arquivo PDF aqui")
                        .size(13)
                        .color(t.muted),
                    button(text("Abrir PDF").size(13))
                        .padding(Padding::from([6, 14]))
                        .style(kiri::hud_primary_style(t))
                        .on_press(Message::PickFile),
                ]
                .spacing(10)
                .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fixed(drop_h))
            .center_x(Length::Fill)
            .center_y(Length::Fixed(drop_h))
            .padding(20),
        ])
        .width(Length::Fill)
        .max_width(560.0)
        .style(kiri::empty_drop_style(t)),
    )
    .width(Length::Fill)
    .center_x(Length::Fill);

    column![
        dropzone,
        header,
        scrollable(recents)
            .width(Length::Fill)
            .height(Length::FillPortion(2)),
        path_row,
        scrollable(listing)
            .width(Length::Fill)
            .height(Length::FillPortion(3)),
    ]
    .spacing(10)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Card de recente: ícone `file-text`, nome do arquivo e pasta-mãe — o único
/// meta que o estado tem (sem tamanho, data ou páginas: não existem aqui).
fn empty_card(path: &std::path::Path, t: Tokens) -> Element<'static, Message> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let mut info = column![text(name).size(13)].spacing(2);
    if let Some(parent) = path.parent().and_then(|p| p.file_name()) {
        info = info.push(
            text(parent.to_string_lossy().into_owned())
                .size(11)
                .color(t.muted),
        );
    }
    container(
        button(
            row![kiri::ori!("file-text"), info]
                .spacing(8)
                .align_y(Alignment::Center)
                .width(Length::Fill),
        )
        .width(Length::Fill)
        .padding(0)
        .style(control_style(t, false))
        .on_press(Message::OpenRecent(path.to_path_buf())),
    )
    .width(Length::Fill)
    .padding(12)
    .style(kiri::recent_card_style(t))
    .into()
}

fn entry_row(entry: &FsEntry, t: Tokens) -> Element<'static, Message> {
    let message = if entry.is_dir {
        Message::BrowseTo(Some(entry.path.clone()))
    } else {
        Message::OpenRecent(entry.path.clone())
    };
    let glyph: Element<'static, Message> = if entry.is_dir {
        kiri::ori!("folder")
    } else {
        kiri::ori!("file-text")
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

fn ready_body(tabs: &Tabs, t: Tokens) -> Element<'_, Message> {
    let ready = tabs.active();
    let mut panes = row![].spacing(12).height(Length::Fill);
    if ready.pages_open {
        panes = panes.push(pages_panel(ready, t));
    }
    panes = panes.push(page_pane(ready, t));
    if ready.signatures_open {
        panes = panes.push(signatures_panel(ready, t));
    }
    // Status pós-ação ("Enviado para …", "Cópia salva em …", falha ao abrir
    // outra aba): 1 linha no topo.
    let status = ready
        .save_status
        .as_deref()
        .or(ready.print_status.as_deref())
        .or(tabs.open_error());
    if let Some(status) = status {
        column![text(status).size(13).color(t.muted), panes,]
            .spacing(8)
            .height(Length::Fill)
            .into()
    } else {
        panes.into()
    }
}

/// Faixa de abas sob a toolbar (issue #40): um botão por documento — o ativo
/// destacado — e o × que fecha a aba. Só existe com 2+ abas.
fn tab_strip(tabs: &Tabs, t: Tokens) -> Element<'_, Message> {
    let mut strip = row![].spacing(4).align_y(Alignment::Center);
    for (index, doc) in tabs.docs().iter().enumerate() {
        let label = short_name(doc.source.path());
        let active = index == tabs.active_index();
        strip = strip.push(
            row![
                tip(
                    control_active(
                        t,
                        button(text(label).size(13)).on_press(Message::SelectTab(index)),
                        active,
                    ),
                    "Trocar para este documento",
                ),
                tip(
                    control(
                        t,
                        button(text("×").size(14).color(t.muted))
                            .on_press(Message::CloseTab(index)),
                    ),
                    "Fechar aba (⌘W)",
                ),
            ]
            .spacing(0)
            .align_y(Alignment::Center),
        );
    }
    container(strip)
        .width(Length::Fill)
        .height(Length::Fixed(TAB_STRIP_HEIGHT))
        .padding(Padding::from([0, 8]))
        .style(move |_| container::Style {
            background: Some(Background::Color(t.chrome)),
            ..container::Style::default()
        })
        .into()
}

/// Nome na aba: cortado para caber na faixa (uma linha; um nome inteiro de
/// caminho longo empurraria as outras abas para fora da janela).
fn short_name(path: &std::path::Path) -> String {
    const MAX: usize = 24;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    if name.chars().count() <= MAX {
        return name;
    }
    let head: String = name.chars().take(MAX - 1).collect();
    format!("{head}…")
}

/// Painel de navegação (Stitch sidebar): cabeçalho `Navegação`, abas segmentadas
/// Miniaturas | Sumário e lista virtualizada (miniaturas ou sumário).
fn pages_panel(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let mut col = column![column![
        text("Navegação").size(13).color(t.ink),
        text("Documento ativo").size(11).color(t.muted),
    ]
    .spacing(2),]
    .spacing(8);
    if ready.outline.is_some() {
        col = col.push(panel_tabs(ready, t));
    }
    if ready.outline.is_some() && ready.outline_open {
        col = col.push(outline_tab(ready, t));
    } else {
        col = col.push(thumbs_tab(ready, t));
    }
    container(col.width(Length::Fill).height(Length::Fill))
        .width(Length::Fixed(PAGES_PANEL_W))
        .height(Length::Fill)
        .padding(6)
        .style(kiri::panel_bg_style(t))
        .into()
}

/// Controle segmentado Miniaturas | Sumário (só quando o PDF tem outline).
fn panel_tabs(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let seg = |label: &'static str, active: bool, msg: Message| {
        button(text(label).size(12))
            .width(Length::Fill)
            .padding(Padding::from([5, 8]))
            .style(kiri::panel_seg_style(t, active))
            .on_press(msg)
    };
    container(row![
        seg(
            "Miniaturas",
            !ready.outline_open,
            Message::OutlineTab(false)
        ),
        seg("Sumário", ready.outline_open, Message::OutlineTab(true)),
    ])
    .padding(Padding::from(2))
    .style(kiri::panel_seg_track_style(t))
    .into()
}
/// Aba Sumário: árvore clicável com expandir/colapsar (body-md + indent) e
/// destaque da ativa.
fn outline_tab(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let active = ready.outline_active();
    let focus = ready.outline_focus();
    let mut col = column![panel_tabs(ready, t)].spacing(8);
    for (path, depth, title, page, has_children) in ready.outline_rows() {
        let is_active = active.as_ref() == Some(&path);
        // O cursor do teclado é a pílula; a página ativa, o texto em accent.
        let is_focus = focus.as_ref() == Some(&path);
        let fold: Element<'_, Message> = if has_children {
            let collapsed = ready.outline_collapsed.contains(&path);
            button(text(if collapsed { "▸" } else { "▾" }).size(11))
                .padding(Padding::from([3, 5]))
                .style(kiri::panel_fold_style(t))
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
            .padding(Padding::from([4, 6]))
            .style(kiri::panel_toc_item_style(t, is_active))
            .on_press(Message::OutlineJump(page)),
            is_focus,
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
        .width(Length::Fill)
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
    let mut col = column![].spacing(8);
    if start > 0 {
        col = col.push(Space::with_height(Length::Fixed(start as f32 * THUMB_ROW)));
    }
    for i in start..end {
        let page = PageNo::from_index(i);
        let preview: Element<'_, Message> = match ready.thumb_surface(page) {
            Some(surface) => image(surface.image.clone())
                .width(Length::Fixed(120.0))
                .into(),
            None => container(Space::new(Length::Fixed(120.0), Length::Fixed(150.0)))
                .style(kiri::panel_thumb_frame_style(t))
                .into(),
        };
        let active = ready.visible == page;
        col = col.push(
            control_active(
                t,
                button(
                    column![
                        // Halo 2px `accent` na ativa (transparente fora) + gap 2px —
                        // borda sempre ocupando o mesmo espaço, sem shift de layout.
                        container(container(preview).style(kiri::panel_thumb_frame_style(t)),)
                            .padding(2)
                            .style(kiri::panel_thumb_style(t, active)),
                        text(format!("Pág. {}", i + 1))
                            .size(11)
                            .font(iced::Font::MONOSPACE)
                            .color(if active { t.accent } else { t.muted }),
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
        .width(Length::Fill)
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
    let sw = ready.sheet_width(ready.visible);
    let stage_w = ready.doc_stage_width(ready.visible);
    let rotated = ready.rotated_media(ready.visible);
    let sh = sw * rotated.height.max(1.0) / rotated.width.max(1.0);
    let page_view: Element<'_, Message> = match ready.visible_surface() {
        Some(surface) => with_marks(
            ready,
            ready.visible,
            sw,
            image(surface.image.clone())
                .width(Length::Fixed(sw))
                .height(Length::Fixed(sh))
                .into(),
        ),
        // Sem bitmap: caixa do tamanho da folha (mesma geometria do
        // placeholder do contínuo) — sem salto de layout quando chega.
        None => {
            let msg = if ready.visible_render_failed() {
                "Não foi possível renderizar esta página."
            } else {
                "Renderizando página…"
            };
            container(text(msg).size(13).color(t.muted))
                .width(Length::Fixed(sw))
                .height(Length::Fixed(sh))
                .center_x(Length::Fixed(sw))
                .align_y(Alignment::Center)
                .into()
        }
    };

    scrollable(
        container(
            // A moldura abraça a folha: só ela escala com o zoom, o palco
            // fora segue estável.
            container(page_view)
                .center_x(Length::Shrink)
                .padding(0)
                .style(kiri::page_frame(&t)),
        )
        .center_x(Length::Fixed(stage_w))
        .padding(Padding {
            top: DOC_PAD_TOP,
            right: DOC_PAD_X,
            bottom: DOC_PAD_BOTTOM,
            left: DOC_PAD_X,
        })
        .style(move |_| container::Style {
            // Canvas do documento: mesmo fundo da janela (Stitch Layer 0).
            background: Some(Background::Color(t.bg)),
            ..container::Style::default()
        }),
    )
    .id(doc_scroll_id())
    .direction(scrollable::Direction::Both {
        vertical: scrollable::Scrollbar::default(),
        horizontal: scrollable::Scrollbar::default(),
    })
    // A rolagem do modo página também entra no estado: o post-it aberto segue
    // a folha (a âncora é janela, a rolagem é o delta).
    .on_scroll(|viewport| Message::DocScrolled(viewport.absolute_offset().y))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Rolagem contínua: coluna de células com a mesma estrutura da página única;
/// fora da janela, placeholders de altura exata (sem montar bitmaps).
fn continuous_pane(ready: &Ready, t: Tokens) -> Element<'_, Message> {
    let total = ready.page_count();
    let (start, end) = ready.doc_window();
    let mut col = column![]
        .spacing(DOC_GAP)
        .width(Length::Fixed(ready.doc_stage_max_width()));
    if start > 0 {
        // Offset acumulado menos um gap (o spacing da coluna já conta um).
        let h = (ready.page_offset(PageNo::from_index(start)) - DOC_GAP).max(0.0);
        col = col.push(Space::with_height(Length::Fixed(h)));
    }
    for i in start..end {
        col = col.push(doc_cell(ready, PageNo::from_index(i), t));
    }
    if end < total {
        let h = (ready.doc_total_height() - ready.page_offset(PageNo::from_index(end)) - DOC_GAP)
            .max(0.0);
        col = col.push(Space::with_height(Length::Fixed(h)));
    }
    scrollable(col)
        .id(doc_scroll_id())
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        })
        .on_scroll(|viewport| Message::DocScrolled(viewport.absolute_offset().y))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn doc_cell(ready: &Ready, page: PageNo, t: Tokens) -> Element<'_, Message> {
    let sw = ready.sheet_width(page);
    let rotated = ready.rotated_media(page);
    let sh = sw * rotated.height.max(1.0) / rotated.width.max(1.0);
    let inner: Element<'_, Message> = match ready.page_surface(page) {
        Some(surface) => with_marks(
            ready,
            page,
            sw,
            image(surface.image.clone())
                .width(Length::Fixed(sw))
                .height(Length::Fixed(sh))
                .into(),
        ),
        None => {
            let h = (ready.doc_cell_height(page, sw) - DOC_PAD_TOP - DOC_PAD_BOTTOM).max(1.0);
            container(text("Renderizando página…").size(13).color(t.muted))
                .width(Length::Fixed(sw))
                .height(Length::Fixed(h))
                .center_x(Length::Fixed(sw))
                .align_y(Alignment::Center)
                .into()
        }
    };
    container(
        // A moldura abraça a folha: só ela escala com o zoom, o palco fora
        // segue estável.
        container(inner)
            .center_x(Length::Shrink)
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
        // Canvas do documento: mesmo fundo da janela (Stitch Layer 0).
        background: Some(Background::Color(t.bg)),
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
    /// Candidato do arrasto de nota: contorno claro pontilhado (o original
    /// segue sólido até o soltar).
    ghost: bool,
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
                                // Canto da folha na janela: a sessão não vê o
                                // layout, e o post-it nasce ancorado nele.
                                sheet: [bounds.x, bounds.y],
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
                    // Solta fora da folha: cancela (sem Up, a âncora do press
                    // e o arrasto de nota ficariam presos).
                    None => (CanvasStatus::Ignored, Some(Message::DragCancelled)),
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
        // Traço do ghost: o mesmo para todos (fatia estática).
        const GHOST_DASH: [f32; 2] = [6.0, 4.0];
        for m in &self.marks {
            let rect = Path::rectangle(Point::new(m.x, m.y), Size::new(m.w, m.h));
            // Ghost do arrasto de nota: contorno claro pontilhado — a nota
            // fica onde está até o soltar.
            if m.ghost {
                frame.fill(&rect, Color::from_rgba(0.99, 0.80, 0.20, 0.25));
                frame.stroke(
                    &rect,
                    Stroke {
                        style: Style::Solid(STICKY),
                        width: 1.5,
                        line_dash: LineDash {
                            segments: &GHOST_DASH,
                            offset: 0,
                        },
                        ..Stroke::default()
                    },
                );
                continue;
            }
            // Nota: amarelo fixo — a folha é sempre branca, o tema não se aplica.
            if m.marker {
                frame.fill(&rect, STICKY);
                frame.stroke(
                    &rect,
                    Stroke {
                        style: Style::Solid(STICKY_INK),
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

/// Foca o editor do post-it recém-aberto (autofocus): o `text_editor` do iced
/// não expõe `Id`, então `operation::focus` não o alcança — aqui o alvo é o
/// único campo focável sem id (e os demais perdem o foco, como no `focus`).
/// A operação roda no quadro seguinte, sobre a árvore que já tem o post-it.
pub fn focus_postit() -> Task<Message> {
    struct FocusEditor;

    impl Operation<Message> for FocusEditor {
        fn focusable(&mut self, state: &mut dyn Focusable, id: Option<&WidgetId>) {
            if id.is_none() {
                state.focus();
            } else {
                state.unfocus();
            }
        }

        fn container(
            &mut self,
            _id: Option<&WidgetId>,
            _bounds: Rectangle,
            operate_on_children: &mut dyn FnMut(&mut dyn Operation<Message>),
        ) {
            operate_on_children(self);
        }
    }

    operate(FocusEditor)
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
                    ghost: false,
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
                ghost: false,
            });
            // Nota: marcador compacto — na origem do primeiro quad ou onde o
            // arrasto o deixou (`Annotation::marker`); o trecho não se move.
            if a.kind == AnnotKind::Note && i == 0 {
                let side = marker_side(&a.quads, media, ready.view_rotation, cw, ch);
                let [mx, my] = display_pt(a.marker_pt(), media, ready.view_rotation, cw, ch);
                marks.push(DrawMark {
                    x: mx,
                    y: my,
                    w: side,
                    h: side,
                    kind: Some(AnnotKind::Note),
                    marker: true,
                    ghost: false,
                });
            }
        }
    }
    // Marcador arrastado: ghost = só o ícone, em contorno pontilhado claro, na
    // posição candidata (o marcador sólido fica onde está até o soltar).
    if let Some((id, marker_pt)) = ready.note_drag_ghost(page) {
        if let Some(note) = ready.annotations.iter().find(|a| a.id == id) {
            let side = marker_side(&note.quads, media, ready.view_rotation, cw, ch);
            let [x, y] = display_pt(marker_pt, media, ready.view_rotation, cw, ch);
            marks.push(DrawMark {
                x,
                y,
                w: side,
                h: side,
                kind: Some(AnnotKind::Note),
                marker: true,
                ghost: true,
            });
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
                .size(12)
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
            // Mesmo veredito do escudo da toolbar (status_dot_color).
            let dot = kiri::status_dot_color(t, std::iter::once(sig.status)).unwrap_or(t.muted);
            col = col.push(
                container(
                    column![
                        row![status_dot(dot), text(name).size(13).color(t.ink),]
                            .spacing(8)
                            .align_y(Alignment::Center),
                        text(status_label(sig.status)).size(12).color(t.ink),
                        text(&sig.status_detail).size(11).color(t.muted),
                    ]
                    .spacing(4),
                )
                .width(Length::Fill)
                .padding(12)
                .style(kiri::panel_card_style(t)),
            );
        }
    }
    container(scrollable(col).height(Length::Fill))
        .width(Length::Fixed(SIG_PANEL_W))
        .height(Length::Fill)
        .padding(6)
        .style(kiri::panel_bg_style(t))
        .into()
}

/// Título de seção de painel: label-caps 10px, maiúsculas, `muted`.
fn section_title(label: &'static str, t: Tokens) -> Element<'static, Message> {
    text(label.to_uppercase()).size(10).color(t.muted).into()
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
