//! Player-owned ASS texture cache. Native parsing/rasterization is the worker's job;
//! the frame thread submits its clock and uploads only a changed completed image.
//! Authored placement is retained even while the transport HUD is visible.
use crate::player::{ass, ass_source, sidecar, text_ass, text_style};
use crate::ui::{
    consts::{SCR_H, SCR_W},
    Painter, Rect,
};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct AssSubtitles {
    frame: Option<Arc<ass::Frame>>,
    source: u64,
    clock: ass_source::Clock,
    textures: Vec<CachedTexture>,
    uploaded: u64,
    spans: Vec<(i32,i32)>,
    dys: Vec<f32>,
    viewport: crate::player::video_geometry::Viewport,
}

impl AssSubtitles {
    pub(crate) fn update(&mut self, ps: &crate::route::PlaybackSession, now: u32) {
        let renderable = crate::route::client_renders_subtitle(ps);
        if renderable && crate::player::loading(ps) {
            if self.source != 0 {
                ass::suspend();
            }
            self.source = 0;
            self.frame = None;
            return;
        }
        let source = if !renderable {
            None
        } else {
            sidecar::ass_source(false)
                .or_else(|| ass_source::selected(crate::player::desired_sub_idx()))
        };
        let Some(source) = source else {
            // A preceding busy interval may already have cleared self.source while keeping
            // native resources. A real Off/no-source transition must release those too.
            ass::clear();
            self.source = 0;
            self.frame = None;
            return;
        };
        if self.source != source.id {
            self.source = source.id;
            self.clock = ass_source::Clock::default();
            self.frame = None;
        }
        let clock = self.clock.sample(
            crate::player::playpos_ns(),
            now,
            crate::player::is_playing(ps),
        );
        let clock = clock.saturating_sub(crate::player::subtitle_offset_ms());
        let (coded_w, coded_h, viewport) = if matches!(source.origin, ass::Origin::Text(_)) {
            (SCR_W as i32, SCR_H as i32, crate::player::video_geometry::Viewport {
                x:0, y:0, width:SCR_W as i32, height:SCR_H as i32,
            })
        } else {
            let (w,h) = crate::player::video_raster();
            (w,h,crate::player::video_viewport(SCR_W as i32, SCR_H as i32))
        };
        if self.viewport != viewport {
            self.viewport = viewport;
            self.frame = None;
        }
        if let Some(frame) =
            ass::request(source, clock, viewport.width, viewport.height, coded_w, coded_h)
        {
            self.frame = Some(frame);
        }
    }

    pub(crate) fn fingerprint(&self) -> u64 {
        self.frame.as_ref().map_or(0, |f| f.serial)
    }

    /// `lift` is the transport being up (or a panel read over it), and `band` the transport's: a
    /// bottom region moves by the band's `lift` plus the `rise` an open section asks, zero with none
    /// (see `lift_dys`). It moves a text subtitle's bottom regions and nothing else: an authored
    /// script keeps its own placement.
    pub(crate) fn draw(&mut self, lift: bool, band: text_style::HudBand) {
        let Some(frame) = &self.frame else {
            if self.source == 0 {
                clear_textures(&mut self.textures);
                self.uploaded = 0;
            }
            return;
        };
        if self.uploaded != frame.serial {
            // Reserve all unchanged regions first. A changing region must not steal
            // a texture that a later static sign still needs in this same frame.
            let matches = retained_regions(&self.textures, &frame.rects);
            let mut old: Vec<_> = std::mem::take(&mut self.textures)
                .into_iter()
                .map(Some)
                .collect();
            let mut next: Vec<_> = matches
                .into_iter()
                .map(|index| index.and_then(|i| old[i].take()))
                .collect();
            for (slot, rect) in next.iter_mut().zip(&frame.rects) {
                if slot.is_none() {
                    let previous = old.iter_mut().find_map(Option::take);
                    let id = nj_base::diag::spans::span("assup", || nj_gfx::gfx::upload_rgba(
                        previous.as_ref().map_or(0, |t| t.id),
                        rect.width,
                        rect.height,
                        rect.rgba.as_ptr(),
                    ));
                    if id != 0 {
                        crate::player::ass_perf::note_upload(rect.rgba.len());
                    }
                    *slot = Some(CachedTexture {
                        id,
                        width: rect.width,
                        height: rect.height,
                        pixels: rect.rgba.clone(),
                    });
                }
            }
            for unused in old.into_iter().flatten() {
                nj_gfx::gfx::delete_tex(unused.id);
            }
            self.textures = next.into_iter().flatten().collect();
            self.uploaded = frame.serial;
        }
        self.spans.clear();
        self.spans.extend(frame.rects.iter().map(|r| (r.y,r.height)));
        lift_dys(frame.origin, lift, band, &self.spans, frame.aligns, &mut self.dys);
        let ink = crate::appkit::player_hud::subtitle_ink();
        for ((texture, rect), dy) in self.textures.iter().zip(&frame.rects).zip(&self.dys) {
            Painter::root().tex(
                texture.id,
                Rect::new(
                    (self.viewport.x + rect.x) as f32,
                    (self.viewport.y + rect.y) as f32 + dy,
                    rect.width as f32,
                    rect.height as f32,
                ),
                0.0,
                ink,
            );
        }
    }

    pub(crate) fn error(&self) -> Option<&'static str> {
        self.frame.as_ref().and_then(|f| f.error).map(crate::player::ass::Fault::message)
    }

    pub(crate) fn release(&mut self) {
        clear_textures(&mut self.textures);
        self.uploaded = 0;
        self.frame = None;
        self.source = 0;
        ass::clear();
    }

    pub(crate) fn render_report(&self) -> crate::ui::frame::RenderReport {
        crate::ui::frame::RenderReport {
            textures: self.textures.iter().filter(|t| t.id != 0).count() as u32,
            bytes: self.textures.iter().map(|t| t.pixels.len()).sum(),
        }
    }
}

/// How far (vertical px, negative is up) each region of a Text-origin frame moves while the
/// transport is up (`lift`); an authored frame never moves. `regions` are `(y, height)` in frame
/// pixels. Every active cue bottom-aligned: all regions move; mixed: the regions at the bottom
/// anchor move unless one of them is a merged top+bottom region. The moving group rises by the
/// band's fixed `lift` and by the `rise` that clears an open section (zero with none), the latter
/// clamped by the safe area (`HudBand::clear_rise`); the whole is clamped once so the group's top
/// stays on the screen.
fn lift_dys(
    origin: ass::Origin,
    lift: bool,
    band: text_style::HudBand,
    regions: &[(i32, i32)],
    aligns: text_ass::AlignSet,
    dys: &mut Vec<f32>,
) {
    dys.clear();
    dys.resize(regions.len(), 0.0);
    let ass::Origin::Text(style) = origin else { return };
    if !lift || !aligns.bottom {
        return;
    }
    let pitch = text_style::CAPTION_LINE_PITCH * style.scale();
    // `dys` doubles as the "moves" mark (-1) until the lift is known.
    if aligns.middle || aligns.top {
        let band = text_style::bottom_anchor_y(style.position) - pitch / 2.0;
        let mut merged = false;
        for (dy, &(y, h)) in dys.iter_mut().zip(regions) {
            if (y + h) as f32 >= band {
                *dy = -1.0;
                merged |= (y as f32) < text_style::SCR_H / 2.0;
            }
        }
        if merged {
            dys.fill(0.0);
            return;
        }
        // A block whose lines are separate regions rises as one: a region sitting right above
        // the moving group (a gap under half a pitch) joins it, up to a fixed point.
        while let Some(top) = moving_top(regions, dys) {
            let mut grew = false;
            for (dy, &(y, h)) in dys.iter_mut().zip(regions) {
                if *dy == 0.0 && y < top && ((y + h) as f32) >= top as f32 - pitch / 2.0 {
                    *dy = -1.0;
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
    } else {
        dys.fill(-1.0);
    }
    let Some(top) = moving_top(regions, dys) else { return };
    // the moving group rises by the band's fixed lift and, while a section is open, by the rise that
    // clears it, unless the group's own top would then cross the safe area (it overlaps the section
    // instead, at its own size). The one shared delta is the same for every region that moves.
    let position = style.position;
    let height = (moving_bottom(regions, dys).unwrap_or(top) - top) as f32;
    let rise = text_style::HudBand::clear_rise(band.rise_for(position), band.rest_limit(position), height);
    let lift = (band.lift + rise).min(top.max(0) as f32);
    for dy in dys.iter_mut().filter(|d| **d != 0.0) {
        *dy = -lift;
    }
}

/// The bottom edge of the regions marked as moving (`dys` entry non-zero).
fn moving_bottom(regions: &[(i32, i32)], dys: &[f32]) -> Option<i32> {
    regions.iter().zip(dys).filter(|(_, &d)| d != 0.0).map(|(r, _)| r.0 + r.1).max()
}

/// The top edge of the regions marked as moving (`dys` entry non-zero).
fn moving_top(regions: &[(i32, i32)], dys: &[f32]) -> Option<i32> {
    regions.iter().zip(dys).filter(|(_, &d)| d != 0.0).map(|(r, _)| r.0).min()
}


struct CachedTexture {
    id: u32,
    width: i32,
    height: i32,
    pixels: Arc<[u8]>,
}

fn clear_textures(textures: &mut Vec<CachedTexture>) {
    for texture in textures.drain(..) {
        nj_gfx::gfx::delete_tex(texture.id);
    }
}

/// The worker interns unchanged RGBA. Pixel identity can therefore retain an
/// upload even if a sign moved or libass reordered its disjoint output regions.
fn retained_regions(textures: &[CachedTexture], rects: &[ass::Rect]) -> Vec<Option<usize>> {
    let mut used = vec![false; textures.len()];
    rects
        .iter()
        .map(|rect| {
            let found = textures.iter().enumerate().position(|(i, texture)| {
                !used[i]
                    && texture.width == rect.width
                    && texture.height == rect.height
                    && Arc::ptr_eq(&texture.pixels, &rect.rgba)
            });
            if let Some(i) = found {
                used[i] = true;
            }
            found
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_lifts_bottom_text_without_moving_authored_or_top_cues() {
        let style=text_style::TextStyle::default();
        let mixed=text_ass::AlignSet {top:true,bottom:true,middle:false};
        let mut dys=Vec::new();
        let regions=[(100,40),(940,40)];
        lift_dys(ass::Origin::Text(style),true,text_style::HudBand::CLASSIC,&regions,mixed,&mut dys);
        assert_eq!(dys,[0.0,-200.0]);
        lift_dys(ass::Origin::Authored,true,text_style::HudBand::CLASSIC,&regions,mixed,&mut dys);
        assert_eq!(dys,[0.0,0.0]);
        lift_dys(ass::Origin::Text(style),true,text_style::HudBand::CLASSIC,&[(100,880)],mixed,&mut dys);
        assert_eq!(dys,[0.0],"a merged top/bottom region conservatively stays still");
        lift_dys(ass::Origin::Text(style),false,text_style::HudBand::CLASSIC,&regions,mixed,&mut dys);
        assert_eq!(dys,[0.0,0.0]);
    }

    const BOTTOM: text_ass::AlignSet = text_ass::AlignSet { top: false, bottom: true, middle: false };
    const TOP_AND_BOTTOM: text_ass::AlignSet = text_ass::AlignSet { top: true, bottom: true, middle: false };

    fn dys_in(band: text_style::HudBand, style: text_style::TextStyle, regions: &[(i32, i32)], aligns: text_ass::AlignSet) -> Vec<f32> {
        let mut dys = Vec::new();
        lift_dys(ass::Origin::Text(style), true, band, regions, aligns, &mut dys);
        dys
    }

    /// **The section rows of the table, through the libass lift**: with Info (block top 668) or
    /// Chapters (684) open, a bottom cue rises by the band's lift AND the rise that clears the section,
    /// so its line ends on the table's limit. libass's bottom anchor (`MarginV` 97) sits 3 px under the
    /// bespoke caption's, as it does with nothing open.
    #[test]
    fn a_bottom_cue_stands_on_the_limit_the_open_section_leaves() {
        use crate::catalog::session::{SubtitlePosition, SubtitleSize};
        let table = [
            (668.0, [656.0, 620.0, 460.0]), // Info
            (684.0, [672.0, 620.0, 460.0]), // Chapters
        ];
        for (top, limits) in table {
            let band = text_style::HudBand::CLASSIC.with_rise(text_style::HudBand::CLASSIC.rise_to_clear(top));
            for (position, limit) in SubtitlePosition::LADDER.into_iter().zip(limits) {
                let trim = text_style::bottom_anchor_y(position) - (text_style::SUB_BASE_Y - text_style::position_lift(position));
                assert_eq!(trim, 3.0);
                for size in SubtitleSize::LADDER {
                    let style = text_style::TextStyle { size, position };
                    let h = (text_style::CAPTION_LINE_PITCH * style.scale()).round() as i32;
                    let bottom = text_style::bottom_anchor_y(position) as i32;
                    let dys = dys_in(band, style, &[(bottom - h, h)], BOTTOM);
                    assert_eq!(bottom as f32 + dys[0] - trim, limit, "{top} {position:?} {size:?}: {dys:?}");
                }
            }
        }
    }

    /// One delta for every region that moves, a region above the limit staying, with a section open.
    #[test]
    fn the_section_rise_is_one_delta_for_the_group_that_moves() {
        let style = text_style::TextStyle::default();
        let band = text_style::HudBand::CLASSIC.with_rise(text_style::HudBand::CLASSIC.rise_to_clear(668.0));
        let (y, h) = (text_style::bottom_anchor_y(style.position) as i32 - 48, 48);
        let regions = [(y - h, h), (y, h), (100, 48)];
        let dys = dys_in(band, style, &regions, TOP_AND_BOTTOM);
        assert_eq!(dys, [-324.0, -324.0, 0.0], "the two lines of the block move together, the top sign stays");
    }

    /// Where the lifted block's top would cross the safe area the rise stops: the block overlaps the
    /// section, the same region, moved.
    #[test]
    fn the_section_rise_stops_where_the_block_would_leave_the_safe_area() {
        use crate::catalog::session::{SubtitlePosition, SubtitleSize};
        let style = text_style::TextStyle { size: SubtitleSize::ExtraLarge, position: SubtitlePosition::Low };
        let height = (3.0 * text_style::CAPTION_LINE_PITCH * style.scale()) as i32;
        let top = text_style::bottom_anchor_y(style.position) as i32 - height;
        // a section 880 px tall would lift the block past the top of the screen
        let band = text_style::HudBand::CLASSIC.with_rise(text_style::HudBand::CLASSIC.rise_to_clear(200.0));
        let dys = dys_in(band, style, &[(top, height)], BOTTOM);
        let lifted = top as f32 + dys[0];
        assert!((lifted - (crate::ui::consts::SAFE.y + 3.0)).abs() < 1.0, "the block stops at the safe top: {lifted}");
        assert_eq!(dys.len(), 1);
    }

    #[test]
    fn moving_and_reordered_regions_retain_uploads_before_recycling_changed_ones() {
        let a: Arc<[u8]> = Arc::from([255, 0, 0, 255]);
        let b: Arc<[u8]> = Arc::from([0, 0, 255, 255]);
        let old = vec![
            CachedTexture {
                id: 11,
                width: 1,
                height: 1,
                pixels: a.clone(),
            },
            CachedTexture {
                id: 12,
                width: 1,
                height: 1,
                pixels: b.clone(),
            },
        ];
        let rect = |x, rgba| ass::Rect {
            x,
            y: 0,
            width: 1,
            height: 1,
            rgba,
        };
        let changed: Arc<[u8]> = Arc::from([0, 255, 0, 255]);
        let next = vec![rect(40, b), rect(90, changed), rect(120, a.clone())];
        assert_eq!(retained_regions(&old, &next), [Some(1), None, Some(0)]);
        assert_eq!(
            retained_regions(&old, &[rect(0, a.clone()), rect(20, a)]),
            [Some(0), None]
        );
        assert!(retained_regions(&old, &[]).is_empty());
    }
}
