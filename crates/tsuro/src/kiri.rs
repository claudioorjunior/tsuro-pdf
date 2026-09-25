//! Kiri (きり) — design tokens do chrome Tsuro.
//!
//! Dark mode é o padrão; light é opt-in via menu ⋯ → Aparência.
//! Folha PDF é sempre branca (`Tokens::page`), independente do tema.
//! Ícones Ori têm cor fixa no SVG — nunca aplique `svg::Style { color }`.

use iced::widget::{button, container, svg, text_input, Space};
use iced::{Background, Border, Color, Length, Shadow, Vector};

/// Tema do chrome. Vive no estado da app, não no `Theme` global do iced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    pub fn is_dark(self) -> bool {
        matches!(self, Theme::Dark)
    }
}

/// Tokens nomeados do Kiri. 15 campos — não mapeiam 1:1 ao `Seed` do
/// `iced::Theme::custom`, por isso vivem aqui + closures `.style()`.
#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub is_dark: bool,
    pub bg: Color,
    pub surface: Color,
    pub chrome: Color,
    pub elevated: Color,
    pub ink: Color,
    pub muted: Color,
    pub line: Color,
    pub accent: Color,
    pub accent_bg: Color,
    pub hover: Color,
    pub ok: Color,
    pub warn: Color,
    pub danger: Color,
    /// Folha PDF — sempre branca, qualquer tema.
    pub page: Color,
    /// Highlight de busca.
    pub mark: Color,
    /// Hit atual da busca (Enter/F3) — âmbar cheio sobre o amarelo claro.
    pub mark_current: Color,
}

impl Tokens {
    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Dark => Self {
                is_dark: true,
                bg: hex(0x18, 0x18, 0x18),
                surface: hex(0x22, 0x22, 0x22),
                chrome: hex(0x1a, 0x1a, 0x1a),
                elevated: hex(0x26, 0x26, 0x26),
                ink: hex(0xec, 0xec, 0xea),
                muted: hex(0x9a, 0x98, 0x90),
                line: hex(0x33, 0x33, 0x31),
                accent: hex(0x4d, 0xb8, 0xb0),
                accent_bg: hex(0x1e, 0x33, 0x31),
                hover: hex(0x2a, 0x2a, 0x28),
                ok: hex(0x5c, 0xb8, 0x5c),
                warn: hex(0xd4, 0xa0, 0x17),
                danger: hex(0xc0, 0x39, 0x2b),
                page: Color::WHITE,
                mark: hex(0xfd, 0xe6, 0x8a),
                mark_current: hex(0xf5, 0x9e, 0x0b),
            },
            Theme::Light => Self {
                is_dark: false,
                bg: hex(0xef, 0xef, 0xed),
                surface: Color::WHITE,
                chrome: hex(0xf7, 0xf7, 0xf6),
                elevated: Color::WHITE,
                ink: hex(0x1c, 0x1c, 0x1a),
                muted: hex(0x78, 0x76, 0x70),
                line: hex(0xe0, 0xde, 0xd9),
                accent: hex(0x1f, 0x7d, 0x76),
                accent_bg: hex(0xe0, 0xf2, 0xf0),
                hover: hex(0xe8, 0xe8, 0xe5),
                ok: hex(0x5c, 0xb8, 0x5c),
                warn: hex(0xd4, 0xa0, 0x17),
                danger: hex(0xc0, 0x39, 0x2b),
                page: Color::WHITE,
                mark: hex(0xfd, 0xe6, 0x8a),
                mark_current: hex(0xf5, 0x9e, 0x0b),
            },
        }
    }
}

fn hex(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

/// Ícone Ori (origami tsuru) — cor fixa no SVG, sem `.style()`.
/// Uso: `ori!("folder-open")` → 16×16. Pílula de busca usa `ori_small!`.
/// V2 (27 ícones): folder, file-text, x, copy, highlighter, underline,
/// strike, note, fit-page, folder-open, search, chevron-left, chevron-right,
/// minus, plus, fit-width, pages, shield, home, more, print, rotate, undo,
/// redo, save, page-single, continuous.
#[allow(unused_macros)]
macro_rules! ori {
    ($file:literal) => {
        $crate::kiri::ori_icon($file, 16.0)
    };
}

#[allow(unused_macros)]
macro_rules! ori_small {
    ($file:literal) => {
        $crate::kiri::ori_icon($file, 15.0)
    };
}

#[allow(unused_imports)]
pub(crate) use ori;
#[allow(unused_imports)]
pub(crate) use ori_small;

pub fn ori_icon<Message: 'static>(file: &str, size: f32) -> iced::Element<'static, Message> {
    let bytes: &'static [u8] = match file {
        "folder-open" => include_bytes!("../assets/icons/ori/folder-open.svg"),
        "search" => include_bytes!("../assets/icons/ori/search.svg"),
        "chevron-left" => include_bytes!("../assets/icons/ori/chevron-left.svg"),
        "chevron-right" => include_bytes!("../assets/icons/ori/chevron-right.svg"),
        "minus" => include_bytes!("../assets/icons/ori/minus.svg"),
        "plus" => include_bytes!("../assets/icons/ori/plus.svg"),
        "fit-width" => include_bytes!("../assets/icons/ori/fit-width.svg"),
        "pages" => include_bytes!("../assets/icons/ori/pages.svg"),
        "shield" => include_bytes!("../assets/icons/ori/shield.svg"),
        "home" => include_bytes!("../assets/icons/ori/home.svg"),
        "more" => include_bytes!("../assets/icons/ori/more.svg"),
        "print" => include_bytes!("../assets/icons/ori/print.svg"),
        // v2
        "folder" => include_bytes!("../assets/icons/ori/folder.svg"),
        "file-text" => include_bytes!("../assets/icons/ori/file-text.svg"),
        "x" => include_bytes!("../assets/icons/ori/x.svg"),
        "copy" => include_bytes!("../assets/icons/ori/copy.svg"),
        "highlighter" => include_bytes!("../assets/icons/ori/highlighter.svg"),
        "underline" => include_bytes!("../assets/icons/ori/underline.svg"),
        "strike" => include_bytes!("../assets/icons/ori/strike.svg"),
        "note" => include_bytes!("../assets/icons/ori/note.svg"),
        "fit-page" => include_bytes!("../assets/icons/ori/fit-page.svg"),
        "rotate" => include_bytes!("../assets/icons/ori/rotate.svg"),
        "undo" => include_bytes!("../assets/icons/ori/undo.svg"),
        "redo" => include_bytes!("../assets/icons/ori/redo.svg"),
        "save" => include_bytes!("../assets/icons/ori/save.svg"),
        "page-single" => include_bytes!("../assets/icons/ori/page-single.svg"),
        "continuous" => include_bytes!("../assets/icons/ori/continuous.svg"),
        _ => include_bytes!("../assets/icons/ori/more.svg"),
    };
    svg(svg::Handle::from_memory(bytes))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}

/// Separador vertical 1×18px da toolbar.
pub fn vsep(tokens: Tokens) -> iced::Element<'static, crate::session::Message> {
    container(Space::with_width(Length::Fixed(1.0)))
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(18.0))
        .padding([2, 0])
        .style(move |_| container::Style {
            background: Some(Background::Color(tokens.line)),
            ..container::Style::default()
        })
        .into()
}

/// Botão-ícone Kiri: fundo transparente, `hover`, `accent_bg` quando ativo.
pub fn ibtn_style(
    tokens: Tokens,
    active: bool,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = if active {
            tokens.accent_bg
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => tokens.hover,
                _ => Color::TRANSPARENT,
            }
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: tokens.ink,
            border: Border {
                radius: 8.0.into(),
                ..Border::default()
            },
            shadow: Shadow::default(),
        }
    }
}

/// Pílula central (busca + página): fundo `elevated`, raio 99px.
pub fn pill_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.elevated)),
        border: Border {
            color: tokens.line,
            width: 1.0,
            radius: 99.0.into(),
        },
        text_color: Some(tokens.ink),
        ..container::Style::default()
    }
}

/// Segmento Kiri (−/+ · ◀▶): fundo `elevated` no dark, `surface` no light, raio 99px.
pub fn seg_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(if tokens.is_dark {
            tokens.elevated
        } else {
            tokens.surface
        })),
        border: Border {
            radius: 99.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// Cor de um status de assinatura. O dot do escudo usa o pior (`status_dot_color`).
pub fn status_color(tokens: Tokens, status: tsuro_sign::SignatureStatus) -> Color {
    use tsuro_sign::SignatureStatus as S;
    match status {
        S::Valid => tokens.ok,
        S::IntactButUntrusted
        | S::Unsupported
        | S::CertificateExpired
        | S::CertificateNotYetValid => tokens.warn,
        S::DocumentModified | S::Invalid => tokens.danger,
    }
}

/// Cor do dot no escudo: pior status vence; `None` sem assinaturas.
pub fn status_dot_color(
    tokens: Tokens,
    statuses: impl IntoIterator<Item = tsuro_sign::SignatureStatus>,
) -> Option<Color> {
    use tsuro_sign::SignatureStatus as S;
    fn rank(s: S) -> u8 {
        match s {
            S::Valid => 1,
            S::IntactButUntrusted
            | S::Unsupported
            | S::CertificateExpired
            | S::CertificateNotYetValid => 2,
            S::DocumentModified | S::Invalid => 3,
        }
    }
    let mut worst: Option<S> = None;
    for s in statuses {
        worst = Some(match worst {
            Some(w) if rank(w) >= rank(s) => w,
            _ => s,
        });
    }
    worst.map(|s| status_color(tokens, s))
}

/// Cartão do menu ⋯: fundo `elevated`, borda `line`, raio 8, sombra leve.
pub fn menu_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.elevated)),
        border: Border {
            color: tokens.line,
            width: 1.0,
            radius: 8.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if tokens.is_dark { 0.35 } else { 0.12 }),
            offset: Vector::new(0.0, 8.0),
            blur_radius: 24.0,
        },
        text_color: Some(tokens.ink),
        ..container::Style::default()
    }
}

/// Item do menu ⋯: texto à esquerda, fundo só no hover.
pub fn menu_item_style(tokens: Tokens) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => tokens.hover,
            _ => Color::TRANSPARENT,
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: tokens.ink,
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            shadow: Shadow::default(),
        }
    }
}

/// Cartão de recente (empty state): fundo `elevated`, raio 8, padding no uso.
pub fn recent_card_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.elevated)),
        border: Border {
            color: tokens.line,
            width: 1.0,
            radius: 8.0.into(),
        },
        text_color: Some(tokens.ink),
        ..container::Style::default()
    }
}

/// Moldura da folha PDF: sempre branca, uma sombra leve.
pub fn page_frame(tokens: &Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    let t = *tokens;
    move |_| container::Style {
        background: Some(Background::Color(t.page)),
        border: Border {
            color: t.line,
            width: 0.0,
            radius: 4.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if t.is_dark { 0.45 } else { 0.08 }),
            offset: Vector::new(0.0, 8.0),
            blur_radius: 24.0,
        },
        ..container::Style::default()
    }
}

// ── Fatia HUD / overlays (Stitch) ──────────────────────────────────────────

/// Pílula do HUD flutuante (Stitch): fundo `elevated`, borda hairline `line`,
/// raio 99 e sombra difusa — 3º nível de elevação, sobre o canvas.
pub fn hud_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.elevated)),
        border: Border {
            color: tokens.line,
            width: 1.0,
            radius: 99.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if tokens.is_dark { 0.45 } else { 0.12 }),
            offset: Vector::new(0.0, 8.0),
            blur_radius: 32.0,
        },
        text_color: Some(tokens.ink),
        ..container::Style::default()
    }
}

/// Ação primária teal do HUD/overlays (Stitch): fundo `accent`, texto escuro
/// (`bg`, que inverte no tema claro), raio 99.
pub fn hud_primary_style(tokens: Tokens) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => lift(tokens.accent, 0.08),
            button::Status::Disabled => Color {
                a: 0.4,
                ..tokens.accent
            },
            _ => tokens.accent,
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: tokens.bg,
            border: Border {
                radius: 99.0.into(),
                ..Border::default()
            },
            shadow: Shadow::default(),
        }
    }
}

/// Ação secundária ghost do HUD/overlays (Stitch): só texto `muted`, que
/// sobe para `ink` com fundo `hover`.
pub fn hud_ghost_style(tokens: Tokens) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let interactive = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if interactive {
                tokens.hover
            } else {
                Color::TRANSPARENT
            })),
            text_color: if interactive {
                tokens.ink
            } else {
                tokens.muted
            },
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            shadow: Shadow::default(),
        }
    }
}

/// Clareia `color` rumo ao branco — hover do primário (Stitch `#5DC7BF`).
fn lift(color: Color, amount: f32) -> Color {
    Color {
        r: color.r + (1.0 - color.r) * amount,
        g: color.g + (1.0 - color.g) * amount,
        b: color.b + (1.0 - color.b) * amount,
        a: color.a,
    }
}

/// Pílula do documento (toolbar): fundo `surface`, hairline `line`, raio 4.
pub fn bar_doc_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.surface)),
        border: Border {
            color: tokens.line,
            width: 1.0,
            radius: 4.0.into(),
        },
        text_color: Some(tokens.ink),
        ..container::Style::default()
    }
}

/// Dot da pílula do documento (marcas ainda não salvas): `accent`, redondo.
/// O tamanho vem do `container` no uso.
pub fn bar_dot_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.accent)),
        border: Border {
            radius: 99.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// Barra de anotar (Destacar · Sublinhar · Riscar · Nota): fundo `elevated`,
/// borda `accent` a 30%, raio 99.
pub fn bar_mark_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.elevated)),
        border: Border {
            color: Color {
                a: 0.3,
                ..tokens.accent
            },
            width: 1.0,
            radius: 99.0.into(),
        },
        text_color: Some(tokens.ink),
        ..container::Style::default()
    }
}

/// Campo de busca/página dentro da pílula: transparente, sem borda própria
/// (a moldura é do `pill_style`), placeholder `muted`.
pub fn bar_input_style(
    tokens: Tokens,
) -> impl Fn(&iced::Theme, text_input::Status) -> text_input::Style {
    move |_theme, _status| text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: tokens.muted,
        placeholder: tokens.muted,
        value: tokens.ink,
        selection: Color {
            a: 0.4,
            ..tokens.accent
        },
    }
}

// ── Fatia estado vazio (Stitch) ────────────────────────────────────────────

/// Dropzone do estado vazio (Stitch): fundo `surface`, raio 12. O tracejado
/// é canvas (`EmptyDash` em `view.rs`) — `Border` do iced não tem dash.
pub fn empty_drop_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.surface)),
        border: Border {
            radius: 12.0.into(),
            ..Border::default()
        },
        text_color: Some(tokens.ink),
        ..container::Style::default()
    }
}

/// Pílula de contagem da seção "Documentos recentes": fundo `elevated`,
/// raio 99, texto `muted` (o tamanho vem do `container` no uso).
pub fn empty_badge_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.elevated)),
        border: Border {
            radius: 99.0.into(),
            ..Border::default()
        },
        text_color: Some(tokens.muted),
        ..container::Style::default()
    }
}

/// Palco do lockup Tsuro: fundo escuro fixo, não segue o tema. O lockup
/// foi desenhado para fundo escuro — sem o palco ele some no tema claro.
pub fn logo_stage_style() -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(Color::from_rgb8(0x18, 0x18, 0x18))),
        border: Border {
            radius: 10.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}
// ── Fatia painéis laterais (Stitch §5 Miniaturas) ─────────────────────────

/// Painel lateral (navegação / assinaturas): fundo chrome, hairline `line`,
/// raio 8 — camada 1 do DESIGN (sidebar docked).
pub fn panel_bg_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.chrome)),
        border: Border {
            color: tokens.line,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    }
}

/// Trilho do segmento Miniaturas | Sumário: fundo `surface` (chrome no light),
/// hairline `line`, raio 6.
pub fn panel_seg_track_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(if tokens.is_dark {
            tokens.surface
        } else {
            tokens.chrome
        })),
        border: Border {
            color: tokens.line,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

/// Segmento do controle Miniaturas|Sumário: `accent_bg`+`accent` quando ativo,
/// `hover` só no hover; raio 4.
pub fn panel_seg_style(
    tokens: Tokens,
    active: bool,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = if active {
            tokens.accent_bg
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => tokens.hover,
                _ => Color::TRANSPARENT,
            }
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: if active { tokens.accent } else { tokens.ink },
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
            shadow: Shadow::default(),
        }
    }
}

/// Halo da miniatura ativa: 2px `accent` (transparente fora da ativa) — a
/// borda ocupa sempre o mesmo espaço, sem shift de layout.
pub fn panel_thumb_style(
    tokens: Tokens,
    active: bool,
) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: None,
        border: Border {
            color: if active {
                tokens.accent
            } else {
                Color::TRANSPARENT
            },
            width: 2.0,
            radius: 5.0.into(),
        },
        ..container::Style::default()
    }
}

/// Moldura da folha na miniatura: branca, 1px `line`, raio 3 (corte de papel).
pub fn panel_thumb_frame_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.page)),
        border: Border {
            color: tokens.line,
            width: 1.0,
            radius: 3.0.into(),
        },
        ..container::Style::default()
    }
}

/// Item do sumário: `accent_bg`+`accent` quando ativo, `hover` no hover;
/// raio 6, texto body-md definido no uso.
pub fn panel_toc_item_style(
    tokens: Tokens,
    active: bool,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = if active {
            tokens.accent_bg
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => tokens.hover,
                _ => Color::TRANSPARENT,
            }
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: if active { tokens.accent } else { tokens.ink },
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            shadow: Shadow::default(),
        }
    }
}

/// Botão ▸/▾ de expandir/colapsar do sumário: quadrado compacto, `hover` suave.
pub fn panel_fold_style(tokens: Tokens) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => tokens.hover,
            _ => Color::TRANSPARENT,
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: tokens.muted,
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            shadow: Shadow::default(),
        }
    }
}

/// Cartão de assinatura (painel lateral): fundo `elevated`, hairline `line`,
/// raio 8 — mesmo vocabulário do cartão de menu/recente.
pub fn panel_card_style(tokens: Tokens) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(tokens.elevated)),
        border: Border {
            color: tokens.line,
            width: 1.0,
            radius: 8.0.into(),
        },
        text_color: Some(tokens.ink),
        ..container::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_is_default_and_page_is_white() {
        let t = Tokens::for_theme(Theme::default());
        assert!(t.is_dark);
        assert_eq!(t.page, Color::WHITE);
        assert_eq!(Tokens::for_theme(Theme::Light).page, Color::WHITE);
    }

    #[test]
    fn chrome_tokens_match_issue_12() {
        let t = Tokens::for_theme(Theme::Dark);
        assert_eq!(t.bg, hex(0x18, 0x18, 0x18));
        assert_eq!(t.accent, hex(0x4d, 0xb8, 0xb0));
        let l = Tokens::for_theme(Theme::Light);
        assert_eq!(l.bg, hex(0xef, 0xef, 0xed));
        assert_eq!(l.elevated, Color::WHITE);
    }

    #[test]
    fn seg_uses_elevated_dark_and_surface_light() {
        let dark = Tokens::for_theme(Theme::Dark);
        let style = seg_style(dark)(&iced::Theme::Dark);
        assert!(matches!(style.background, Some(Background::Color(c)) if c == dark.elevated));
        let light = Tokens::for_theme(Theme::Light);
        let style = seg_style(light)(&iced::Theme::Light);
        assert!(matches!(style.background, Some(Background::Color(c)) if c == light.surface));
    }

    #[test]
    fn dot_worst_status_wins_and_empty_has_no_dot() {
        use tsuro_sign::SignatureStatus as S;
        let t = Tokens::for_theme(Theme::Dark);
        assert_eq!(status_dot_color(t, []), None);
        assert_eq!(status_dot_color(t, [S::Valid]), Some(t.ok));
        assert_eq!(status_dot_color(t, [S::IntactButUntrusted]), Some(t.warn));
        assert_eq!(
            status_dot_color(t, [S::Valid, S::CertificateExpired]),
            Some(t.warn)
        );
        assert_eq!(
            status_dot_color(t, [S::Valid, S::IntactButUntrusted, S::Invalid]),
            Some(t.danger)
        );
        assert_eq!(status_dot_color(t, [S::DocumentModified]), Some(t.danger));
    }

    #[test]
    fn search_current_differs_from_mark_in_both_themes() {
        for theme in [Theme::Dark, Theme::Light] {
            let t = Tokens::for_theme(theme);
            assert_ne!(t.mark_current, t.mark);
            assert_ne!(t.mark_current, t.page);
        }
    }
}
