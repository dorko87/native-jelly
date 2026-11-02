//! Shared caption geometry for synthesized ASS and the HUD's subtitle notices.
//! Values preserve the existing size/position ladder on the 1920x1080 UI canvas.
use crate::catalog::session::{SubtitlePosition, SubtitleSize};

pub(crate) const SCR_W: f32 = nj_base::surface::LOGICAL_W;
pub(crate) const SCR_H: f32 = nj_base::surface::LOGICAL_H;
pub(crate) const CAPTION_FACE: f32 = 36.0;
pub(crate) const CAPTION_LINE_PITCH: f32 = 48.0;
const CAPTION_WRAP_W: f32 = 760.0;
const CAPTION_MAX_W: f32 = SCR_W * 0.75;
pub(crate) const SUB_BASE_Y: f32 = SCR_H - 100.0;
pub(crate) const SUB_CEIL_Y: f32 = SCR_H - 300.0;
pub(crate) const HUD_LIFT: f32 = SUB_BASE_Y - SUB_CEIL_Y;
pub(crate) const TOP_MARGIN_V: f32 = 100.0;
/// The gap a caption lifted clear of an open section keeps above it (the same 12 px the
/// ceiling keeps above the transport's control row, 792 - 780).
pub(crate) const CAPTION_INK_CLEARANCE: f32 = 12.0;
/// The top of the safe area (`ui::consts::SAFE`'s y, which media does not name): a caption lifted
/// clear of a section never rises past it ([`HudBand::clear_rise`]).
pub(crate) const CAPTION_SAFE_TOP_Y: f32 = 54.0;
// libass measures Inter Bold through its OS/2 win metrics (2929 / 2048),
// while the old caption face used em pixels. Keep that conversion explicit.
const INTER_BOLD_WIN_EM: f64 = 2929.0 / 2048.0;

pub(crate) fn size_scale(size: SubtitleSize) -> f32 {
    match size {
        SubtitleSize::Small => 0.78,
        SubtitleSize::Medium => 1.0,
        SubtitleSize::Large => 1.25,
        SubtitleSize::ExtraLarge => 1.5,
        SubtitleSize::Huge => 1.8,
    }
}
pub(crate) fn position_lift(position: SubtitlePosition) -> f32 {
    match position {
        SubtitlePosition::Low => 0.0,
        SubtitlePosition::Middle => 160.0,
        SubtitlePosition::High => 320.0,
    }
}
pub(crate) fn side_margin(scale: f32) -> u32 {
    ((SCR_W - (CAPTION_WRAP_W * scale).min(CAPTION_MAX_W)) / 2.0).round() as u32
}
pub(crate) fn bottom_margin_v(position: SubtitlePosition) -> u32 {
    (97.0 + position_lift(position)).round() as u32
}
pub(crate) fn bottom_anchor_y(position: SubtitlePosition) -> f32 {
    SCR_H - bottom_margin_v(position) as f32
}
pub(crate) fn font_size(scale: f32) -> f64 {
    CAPTION_FACE as f64 * scale as f64 * INTER_BOLD_WIN_EM
}
pub(crate) fn line_spacing(scale: f32) -> f64 {
    CAPTION_LINE_PITCH as f64 * scale as f64 - font_size(scale)
}

/// **The band the transport occupies at the bottom of the screen**, as the numbers every caption
/// reader needs: the ceiling a lifted caption is held under (`ceil_y`), the fixed distance a text
/// caption moves up while the transport is up (`lift`) and, while a player section (Info,
/// Chapters) is open, how much further a caption has risen to clear it (`rise`). The readers
/// (`player_hud::caption_baseline`, `player_hud::hud_lift`, the libass `lift_dys`) take it as an
/// argument; the player screen builds it each frame ([`HudBand::with_rise`]).
///
/// **The rise**: a caption's bottom limit is the transport-up limit P of the viewer's Position
/// (`ceil_y - position_lift`) while no section is open, and `min(P, S - CAPTION_INK_CLEARANCE)`
/// while one is, `S` being the resting top of what the section lays out. `rise[i]` is `P - limit`
/// for Position `i` in [`SubtitlePosition::LADDER`] order (Low, Middle, High), so a Position that
/// already clears the section has none, and a band with every `rise` zero leaves every reader's
/// arithmetic as it was. The rise is motion (a spring the player steps,
/// `appkit::player_hud::caption_rise`), which is why it is a field of the value the readers take
/// and not something they compute.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HudBand {
    pub ceil_y: f32,
    pub lift: f32,
    pub rise: [f32; 3],
}

impl HudBand {
    /// The transport's band with nothing open: ceiling [`SUB_CEIL_Y`], lift [`HUD_LIFT`].
    pub(crate) const CLASSIC: Self = Self { ceil_y: SUB_CEIL_Y, lift: HUD_LIFT, rise: [0.0; 3] };

    /// This band with the rise a section has lifted the captions by (one entry per Position).
    pub(crate) const fn with_rise(self, rise: [f32; 3]) -> Self {
        Self { rise, ..self }
    }

    /// P: where the transport-up limit of `position` sits with no section open.
    pub(crate) fn rest_limit(&self, position: SubtitlePosition) -> f32 {
        self.ceil_y - position_lift(position)
    }

    /// The rise of `position`'s limit, before the safe-area clamp ([`clear_rise`](Self::clear_rise)).
    pub(crate) fn rise_for(&self, position: SubtitlePosition) -> f32 {
        self.rise.get(position.index() as usize).copied().unwrap_or(0.0)
    }

    /// The rise each Position takes to stand `CAPTION_INK_CLEARANCE` above a block whose top edge is
    /// `block_top`: `max(0, P - (block_top - CAPTION_INK_CLEARANCE))`. A Position already above that
    /// takes none (Position gives way first).
    pub(crate) fn rise_to_clear(&self, block_top: f32) -> [f32; 3] {
        SubtitlePosition::LADDER.map(|p| (self.rest_limit(p) - (block_top - CAPTION_INK_CLEARANCE)).max(0.0))
    }

    /// **The safe-area clamp**: of the `rise` a caption of `height` px was given, the part that keeps
    /// its top at or under [`CAPTION_SAFE_TOP_Y`] when its bottom stands on `rest - rise`. A caption
    /// that would cross it stops there and overlaps the section instead (captions are drawn above
    /// sections); its size never gives way. A caption already past the safe top at `rest` is not
    /// moved further, and a zero rise stays exactly zero.
    pub(crate) fn clear_rise(rise: f32, rest: f32, height: f32) -> f32 {
        rise.min((rest - CAPTION_SAFE_TOP_Y - height).max(0.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextStyle {
    pub size: SubtitleSize,
    pub position: SubtitlePosition,
}
impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: SubtitleSize::Medium,
            position: SubtitlePosition::Low,
        }
    }
}
impl TextStyle {
    pub(crate) fn current() -> Self {
        Self {
            size: crate::route::subtitle_size(),
            position: crate::route::subtitle_position(),
        }
    }
    pub(crate) fn scale(&self) -> f32 {
        size_scale(self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small to Extra large keep their scales; Huge (1.8, 65 px) is the rung above. Its wrap width
    /// is 760 * 1.8 = 1368 px, under the 75% cap, so an ordinary two-line cue stays two lines.
    #[test]
    fn huge_is_the_rung_above_extra_large_and_wraps_inside_the_cap() {
        let scales: Vec<f32> = SubtitleSize::LADDER.iter().map(|s| size_scale(*s)).collect();
        assert_eq!(scales, [0.78, 1.0, 1.25, 1.5, 1.8]);
        let margins: Vec<u32> = scales.iter().map(|&s| side_margin(s)).collect();
        assert_eq!(margins, [664, 580, 485, 390, 276]);
        assert_eq!((CAPTION_FACE * size_scale(SubtitleSize::Huge)).round(), 65.0);
        assert_eq!(side_margin(10.0), 240, "any size is capped at 75% of the width");
    }

    #[test]
    fn classic_band_is_the_transport_ceiling_with_nothing_open() {
        assert_eq!(HudBand::CLASSIC, HudBand { ceil_y: 780.0, lift: 200.0, rise: [0.0; 3] });
        assert_eq!(HudBand::CLASSIC.ceil_y, SUB_CEIL_Y);
        assert_eq!(HudBand::CLASSIC.lift, HUD_LIFT);
    }

    /// The rise a section asks of each Position: `max(0, P - (S - 12))`, so a Position that already
    /// clears the block takes none, and the safe-area clamp leaves a zero rise at zero.
    #[test]
    fn the_rise_clears_the_block_and_gives_way_by_position() {
        let band = HudBand::CLASSIC;
        let rise = |top: f32| band.rise_to_clear(top);
        assert_eq!(rise(1080.0), [0.0; 3], "no block");
        assert_eq!(rise(668.0), [124.0, 0.0, 0.0], "Info: only Low is in the way");
        assert_eq!(rise(684.0), [108.0, 0.0, 0.0], "Chapters");
        assert_eq!(rise(600.0), [192.0, 32.0, 0.0], "a taller block reaches Middle too");
        assert_eq!(rise(480.0), [312.0, 152.0, 0.0]);
        assert_eq!(rise(300.0), [492.0, 332.0, 172.0], "…and High");
        assert_eq!(band.with_rise(rise(668.0)).rise_for(SubtitlePosition::Low), 124.0);
        assert_eq!(HudBand::clear_rise(0.0, 780.0, 1000.0), 0.0, "no rise stays none, whatever the block");
        assert_eq!(HudBand::clear_rise(124.0, 780.0, 144.0), 124.0, "a block that fits keeps its rise");
        assert_eq!(HudBand::clear_rise(592.0, 780.0, 216.0), 780.0 - CAPTION_SAFE_TOP_Y - 216.0, "top stops on the safe area");
        assert_eq!(CAPTION_SAFE_TOP_Y, 54.0);
    }
}
