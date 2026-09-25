//! Busca textual no documento: consulta, hits e mapeamento de bytes.
//!
//! Extraído de `session.rs` sem mudança de comportamento: `Search` guarda a
//! consulta e os hits ordenados por (página, byte inicial); `find_hits` acha
//! ranges case-insensitive em `TextLayer.plain` e resolve cada um para um
//! `Quad` caminhando os clusters dos glifos.

use crate::page::{Glyph, PageNo, Quad, TextLayer};
use crate::session::TextRange;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone)]
pub struct Search {
    query: String,
    hits: Vec<Hit>,
    current: Option<usize>,
}

impl Search {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// Índice do hit atual (Enter/F3); `None` = digitando, sem seleção.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    pub fn current_hit(&self) -> Option<&Hit> {
        self.current.and_then(|i| self.hits.get(i))
    }

    /// Passo com wrap: do vazio, +1 vai ao primeiro e −1 ao último.
    pub(crate) fn step(&mut self, delta: i32) {
        if self.hits.is_empty() {
            self.current = None;
            return;
        }
        let len = self.hits.len() as i32;
        let next = match self.current {
            None if delta >= 0 => 0,
            None => len - 1,
            Some(i) => (i as i32 + delta).rem_euclid(len),
        };
        self.current = Some(next as usize);
    }

    pub(crate) fn derive(query: &str, pages: &[Option<TextLayer>]) -> Self {
        if query.is_empty() {
            return Search {
                query: query.to_string(),
                hits: Vec::new(),
                current: None,
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
            current: None,
        }
    }

    pub(crate) fn extend_page(&mut self, layer: &TextLayer) {
        if self.query.is_empty() {
            return;
        }
        let page_idx = layer.page.index();
        let cur = self.current_hit().map(|hit| (hit.page, hit.range));
        self.hits.retain(|hit| hit.page != layer.page);
        let mut page_hits = find_hits(&self.query, layer);
        page_hits.sort_by_key(|hit| hit.range.start);
        let pos = self
            .hits
            .partition_point(|hit| (hit.page.index(), hit.range.start) < (page_idx, 0));
        self.hits.splice(pos..pos, page_hits);
        self.current = cur.and_then(|(page, range)| {
            self.hits
                .iter()
                .position(|hit| hit.page == page && hit.range == range)
        });
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub page: PageNo,
    pub range: TextRange,
    pub quad: Quad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LowerChar {
    lower: char,
    byte_start: usize,
    byte_end: usize,
}

fn lower_plain_map(plain: &str) -> Vec<LowerChar> {
    // NFC antes de minusculizar (#79b): o Pdfium entrega um `char` por
    // `unicode_string`, então o `plain` pode guardar NFD (`e` + combining)
    // enquanto o teclado entrega NFC (`é`). Compor aqui alinha os dois
    // lados sem mexer nos bytes — `byte_start/end` continuam no `plain`.
    let mut mapped = Vec::new();
    let mut cursor = plain;
    for ch in plain.chars().nfc() {
        // Consome do `plain` os chars que compõem `ch`: 1 em NFC, 2+ em
        // NFD (`e` + combining). O range cobre os bytes originais.
        let mut width = 0usize;
        let mut composed: String = String::new();
        for raw in cursor.chars() {
            width += raw.len_utf8();
            composed.push(raw);
            if composed.chars().nfc().collect::<String>() == ch.to_string() {
                break;
            }
        }
        let byte_start = plain.len() - cursor.len();
        cursor = &cursor[width.min(cursor.len())..];
        let byte_end = byte_start + width;
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
    let needle_nfc: String = needle.chars().nfc().collect();
    let needle_chars: Vec<char> = needle_nfc
        .chars()
        .flat_map(|ch| ch.to_lowercase())
        .collect();
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

pub(crate) fn quads_for_range_monotonic(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::{Glyph, PageNo, Quad, TextLayer};

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
    fn step_wraps_and_first_prev_goes_last() {
        let layer = TextLayer {
            page: PageNo::first(),
            plain: "a a a".into(),
            glyphs: vec![Glyph {
                cluster: "a a a".into(),
                quad: Quad::from_rect(0.0, 0.0, 10.0, 10.0),
            }],
        };
        let mut search = Search::derive("a", &[Some(layer)]);
        assert_eq!(search.hits().len(), 3);
        assert_eq!(search.current(), None);
        search.step(1);
        assert_eq!(search.current(), Some(0));
        search.step(1);
        assert_eq!(search.current(), Some(1));
        search.step(1);
        assert_eq!(search.current(), Some(2));
        search.step(1);
        assert_eq!(search.current(), Some(0), "wrap no fim");
        search.step(-1);
        assert_eq!(search.current(), Some(2), "wrap no início");
        let mut fresh = Search::derive(
            "a",
            &[Some(TextLayer {
                page: PageNo::first(),
                plain: "a a".into(),
                glyphs: vec![Glyph {
                    cluster: "a a".into(),
                    quad: Quad::from_rect(0.0, 0.0, 10.0, 10.0),
                }],
            })],
        );
        fresh.step(-1);
        assert_eq!(fresh.current(), Some(1), "Shift+Enter abre no último");
        let mut empty = Search::derive("", &[]);
        empty.step(1);
        assert_eq!(empty.current(), None);
    }

    #[test]
    fn extend_page_preserves_current_hit() {
        let first = TextLayer {
            page: PageNo::first(),
            plain: "nada aqui".into(),
            glyphs: vec![Glyph {
                cluster: "nada aqui".into(),
                quad: Quad::from_rect(0.0, 0.0, 10.0, 10.0),
            }],
        };
        let mut search = Search::derive("alvo", &[Some(first)]);
        assert!(search.hits().is_empty());
        let second = TextLayer {
            page: PageNo::from_index(1),
            plain: "um alvo só".into(),
            glyphs: vec![Glyph {
                cluster: "um alvo só".into(),
                quad: Quad::from_rect(0.0, 0.0, 10.0, 10.0),
            }],
        };
        search.extend_page(&second);
        assert_eq!(search.hits().len(), 1);
        search.step(1);
        assert_eq!(search.current(), Some(0));
        // Página anterior chega depois: o hit atual segue o mesmo trecho.
        let zero = TextLayer {
            page: PageNo::first(),
            plain: "alvo no início".into(),
            glyphs: vec![Glyph {
                cluster: "alvo no início".into(),
                quad: Quad::from_rect(0.0, 0.0, 10.0, 10.0),
            }],
        };
        search.extend_page(&zero);
        assert_eq!(search.hits().len(), 2);
        assert_eq!(search.current(), Some(1));
        assert_eq!(
            search.current_hit().map(|hit| hit.page),
            Some(PageNo::from_index(1))
        );
    }

    #[test]
    fn decomposed_acute_keeps_x_quad_on_x() {
        // #79: `e` + combining + `x` sem a segunda NFC — o `x` cai no glifo
        // do `x`, nunca no acento.
        let e = Quad::from_rect(0.0, 0.0, 10.0, 10.0);
        let accent = Quad::from_rect(10.0, 0.0, 20.0, 10.0);
        let x = Quad::from_rect(20.0, 0.0, 30.0, 10.0);
        let layer = TextLayer {
            page: PageNo::first(),
            plain: "e\u{301}x".into(),
            glyphs: vec![
                Glyph {
                    cluster: "e".into(),
                    quad: e,
                },
                Glyph {
                    cluster: "\u{301}".into(),
                    quad: accent,
                },
                Glyph {
                    cluster: "x".into(),
                    quad: x,
                },
            ],
        };
        assert_eq!(layer.plain.len(), 4, "sem NFC o plain soma os clusters");
        let hits = find_hits("x", &layer);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].quad.x0, hits[0].quad.x1), (20.0, 30.0));
    }

    #[test]
    fn nfc_query_matches_nfd_text() {
        let layer = TextLayer {
            page: PageNo::first(),
            plain: "e\u{301}x".into(),
            glyphs: vec![Glyph {
                cluster: "e\u{301}x".into(),
                quad: Quad::from_rect(0.0, 0.0, 30.0, 10.0),
            }],
        };
        let hits = find_hits("\u{e9}", &layer);
        assert_eq!(hits.len(), 1, "NFC é vs NFD e+combining");
    }

    #[test]
    fn nfd_query_matches_nfc_text() {
        let layer = TextLayer {
            page: PageNo::first(),
            plain: "\u{e9}x".into(),
            glyphs: vec![Glyph {
                cluster: "\u{e9}x".into(),
                quad: Quad::from_rect(0.0, 0.0, 30.0, 10.0),
            }],
        };
        let hits = find_hits("e\u{301}", &layer);
        assert_eq!(hits.len(), 1, "NFD e+combining vs NFC é");
    }
}
