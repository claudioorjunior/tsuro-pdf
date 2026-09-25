//! Busca textual no documento: consulta, hits e mapeamento de bytes.
//!
//! Extraído de `session.rs` sem mudança de comportamento: `Search` guarda a
//! consulta e os hits ordenados por (página, byte inicial); `find_hits` acha
//! ranges case-insensitive em `TextLayer.plain` e resolve cada um para um
//! `Quad` caminhando os clusters dos glifos.

use crate::page::{Glyph, PageNo, Quad, TextLayer};
use crate::session::TextRange;

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

    pub(crate) fn derive(query: &str, pages: &[Option<TextLayer>]) -> Self {
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

    pub(crate) fn extend_page(&mut self, layer: &TextLayer) {
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
}
