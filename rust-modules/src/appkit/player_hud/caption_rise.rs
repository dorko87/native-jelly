//! **How far the captions have risen to clear an open section** (`docs/text-subtitles.md`): the
//! Info card and the Chapters strip are surfaces on the player page's `ModalStack`, and while one
//! is open a caption stands clear of it. [`CaptionRise`] is a field of the player screen, stepped
//! once per frame after the container has stepped the sections' appear springs, and its presented
//! value reaches the caption readers inside the [`HudBand`] they take ([`HudBand::with_rise`]), so
//! no writer into the media layer exists.
//!
//! **The rule.** A caption's bottom limit is `min(P, S - 12)` while any section is open: P is the
//! transport-up limit of the viewer's Position and S the resting top of what the open sections lay
//! out ([`OpenSections::top`]). It is kept as a RISE, `P - limit` per Position
//! ([`HudBand::rise_to_clear`]), so the three Positions are three springs on one shared stiffness:
//! a single spring on S would leave a Middle or High caption waiting for the section to reach it and
//! then creeping, where each Position's own limit moves from the first appear frame.
//!
//! **Rise only.** While sections stay open the target only grows: the block top held is the lowest S
//! of every frame since the first section opened ([`CaptionRise::follow`]), so a second section or a
//! taller one lifts the limit, a shorter one or the close of one of two holds it, and the last close
//! sends it home.
//!
//! **Motion.** `nj_machine::motion::spring` on the surfaces' own appear stiffness
//! ([`APPEAR_K`], 300), the critically damped closed form the sections' appear uses, so it cannot
//! pass either end. It takes its first step on the frame the section's appear takes its first
//! ([`CaptionRise::step`]'s `held`, and the dismissal frame [`CaptionRise::follow`] notes): held while
//! any section's spring is still held at appear 0 and on the frame a section was dismissed, on open
//! and on close alike.
use crate::player::text_style::HudBand;
use crate::ui::containers::modal::APPEAR_K;
use nj_machine::machine::{PresentHandle, Tick};
use nj_machine::motion;

/// One section surface as the container holds it this frame: what [`OpenSections::fold`] reads. The
/// player's bridge builds it from each Info or Chapters surface that is not `Hidden`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SectionSurface {
    /// The section's bit in the open set.
    pub(crate) bit: u8,
    /// The resting top edge of what the section lays out.
    pub(crate) top: f32,
    /// Is it open (`Opening` or `Open`), as against fading out (`Closing`)?
    pub(crate) open: bool,
    /// Is its appear spring held at 0 (the presented frame and any frames its hold is extended by)?
    pub(crate) held: bool,
    /// Its appear spring's value this frame.
    pub(crate) appear: f32,
}

/// **The sections over the player this frame**, folded from the container's surfaces.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct OpenSections {
    /// The bits of the sections that are open (`0` when none is).
    pub(crate) open: u8,
    /// The resting top of everything the open sections lay out, `None` with none open.
    pub(crate) top: Option<f32>,
    /// Is any section's appear spring held at 0 this frame?
    pub(crate) held: bool,
    /// **Does any section draw something this frame?** True while a section's appear is above 0:
    /// not on the frames it is held at appear 0 and not once its spring has landed on 0 after a
    /// close. What the caption draw order follows, so a caption changes layers only while a section
    /// is actually on screen.
    pub(crate) visible: bool,
}

impl OpenSections {
    pub(crate) fn fold(surfaces: impl IntoIterator<Item = SectionSurface>) -> Self {
        let mut out = Self::default();
        for s in surfaces {
            if s.open {
                out.open |= s.bit;
                out.top = Some(out.top.map_or(s.top, |top| top.min(s.top)));
            }
            out.held |= s.held;
            out.visible |= s.appear > 0.0;
        }
        out
    }
}

/// The rise per Position (Low, Middle, High) and the spring that moves it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CaptionRise {
    rise: [f32; 3],
    vel: [f32; 3],
    target: [f32; 3],
    /// The lowest block top of every frame while a section has been open, `None` with none open.
    held_top: Option<f32>,
    /// The `ceil_y` the rest value was built on (NaN until the first call).
    ceil: f32,
    /// The open set the last [`follow`](Self::follow) saw.
    open: u8,
    /// A section left the open set on this frame: its appear takes no step until the next.
    dismissed: bool,
}

impl Default for CaptionRise {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptionRise {
    pub(crate) const fn new() -> Self {
        Self {
            rise: [0.0; 3],
            vel: [0.0; 3],
            target: [0.0; 3],
            held_top: None,
            ceil: f32::NAN,
            open: 0,
            dismissed: false,
        }
    }

    /// **Aim the springs** at the sections that are open now. `open` is [`OpenSections::open`] (`0`
    /// when none is), `top` the resting top of what they lay out (`None` with none open), `band` the
    /// transport's own.
    ///
    /// Nothing open: the block held is forgotten and the target is home. Otherwise the block held can
    /// only get taller, so the target can only grow. A band that is not the one the springs were
    /// built for puts them back at rest, once, with no section open. A section that left the open set
    /// since the last call was dismissed on this frame ([`step`](Self::step) waits for it).
    pub(crate) fn follow(&mut self, open: u8, band: HudBand, top: Option<f32>) {
        self.dismissed = self.open & !open != 0;
        self.open = open;
        if open == 0 {
            self.held_top = None;
        } else if let Some(top) = top {
            self.held_top = Some(self.held_top.map_or(top, |held| held.min(top)));
        }
        if self.ceil != band.ceil_y {
            self.ceil = band.ceil_y;
            if self.held_top.is_none() {
                *self = Self { ceil: band.ceil_y, open: self.open, dismissed: self.dismissed, ..Self::new() };
            }
        }
        self.target = self.held_top.map_or([0.0; 3], |held| band.rise_to_clear(held));
    }

    /// **Step the springs**, once per frame after [`follow`](Self::follow). `held` is true on the
    /// frames a section's appear spring is held at 0; the frame a section was dismissed on waits too,
    /// so the first frame the limit moves is the first frame the section does.
    pub(crate) fn step(&mut self, t: Tick, present: &mut PresentHandle<'_>, held: bool) {
        if held || self.dismissed {
            return;
        }
        for i in 0..3 {
            motion::spring(&mut self.rise[i], &mut self.vel[i], self.target[i], APPEAR_K, t, present);
            if !motion::moving(self.rise[i], self.target[i], self.vel[i], t.dt()) {
                let changed = self.rise[i] != self.target[i] || self.vel[i] != 0.0;
                self.rise[i] = self.target[i];
                self.vel[i] = 0.0;
                if changed {
                    present.note(nj_machine::present::PresentEvent::Motion);
                }
            }
            self.rise[i] = self.rise[i].max(0.0);
        }
    }

    /// The rise the captions are drawn at this frame, per Position.
    pub(crate) fn presented(&self) -> [f32; 3] {
        self.rise
    }

    /// Where the springs are headed (a test reads it; nothing in a draw does).
    #[cfg(test)]
    pub(crate) fn target(&self) -> [f32; 3] {
        self.target
    }
}

/// **The captions rise clear of an open section**: the limit is `min(P, S - 12)`, kept as a rise per
/// Position, moved on the sections' own appear stiffness, and only ever raised while a section stays
/// open. The rig drives the container's real appear spring ([`PopoverMotion`]) the way `ModalStack`
/// does — presented at 0 and held one frame, stepped before the frame's keys, `to(0)` on a dismissal —
/// and reads the limit back through the real readers.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::appkit::player_hud::{caption_baseline, hud_lift, overhangs};
    use crate::catalog::session::SubtitlePosition as Pos;
    use crate::ui::containers::modal::PopoverMotion;
    use crate::ui::Rect;
    use nj_machine::present::Present;

    const LOW: usize = 0;
    const INFO: u8 = 1;
    const CHAPTERS: u8 = 2;
    /// The resting tops of NJ's two sections: the Info card (`SCR_H - 176 - 236`) and the Chapters
    /// strip (`CH_TOP`); `screens::player`'s own test pins them against the panels.
    const INFO_TOP: f32 = 668.0;
    const CHAPTERS_TOP: f32 = 684.0;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    struct Surface {
        bit: u8,
        top: f32,
        open: bool,
        motion: PopoverMotion,
    }

    /// The player's own sequence: the container steps every surface, a key lands (present or
    /// dismiss), and then the player folds the surfaces and steps the rise.
    struct Rig {
        surfaces: Vec<Surface>,
        rise: CaptionRise,
        ms: u32,
        present: Present,
    }

    impl Rig {
        fn new() -> Self {
            Self { surfaces: Vec::new(), rise: CaptionRise::new(), ms: 1_000, present: Present::new() }
        }
        fn tick(&self) -> Tick {
            Tick { ms: self.ms, dt_us: 16_667 }
        }
        /// The container's step: every surface's appear, a settled Closing one pruned.
        fn containers(&mut self) {
            let t = self.tick();
            let mut ph = PresentHandle::of(&mut self.present);
            for s in &mut self.surfaces {
                s.motion.tick_gated(t, &mut ph, false);
            }
            self.surfaces.retain(|s| s.open || !s.motion.settled());
        }
        /// The player's step: fold, aim and step the rise.
        fn player(&mut self) {
            let now = OpenSections::fold(self.surfaces.iter().map(|s| SectionSurface {
                bit: s.bit,
                top: s.top,
                open: s.open,
                held: s.motion.held(),
                appear: s.motion.appear,
            }));
            self.rise.follow(now.open, HudBand::CLASSIC, now.top);
            let t = self.tick();
            let mut ph = PresentHandle::of(&mut self.present);
            self.rise.step(t, &mut ph, now.held);
            self.ms += 16;
        }
        /// A frame on which `key` lands after the containers stepped.
        fn frame_with(&mut self, key: impl FnOnce(&mut Self)) {
            self.containers();
            key(self);
            self.player();
        }
        fn frame(&mut self) {
            self.frame_with(|_| {});
        }
        fn open(&mut self, bit: u8) {
            self.frame_with(|rig| {
                let top = if bit == INFO { INFO_TOP } else { CHAPTERS_TOP };
                let mut motion = PopoverMotion::at(0.0);
                motion.to(1.0);
                motion.hold_one_frame();
                rig.surfaces.push(Surface { bit, top, open: true, motion });
            });
        }
        fn close(&mut self, bit: u8) {
            self.frame_with(|rig| {
                for s in rig.surfaces.iter_mut().filter(|s| s.bit == bit && s.open) {
                    s.open = false;
                    s.motion.to(0.0);
                }
            });
        }
        fn settle(&mut self) {
            for _ in 0..300 {
                self.frame();
            }
        }
        fn appear(&self, bit: u8) -> f32 {
            self.surfaces.iter().find(|s| s.bit == bit).map_or(0.0, |s| s.motion.appear)
        }
        fn band(&self) -> HudBand {
            HudBand::CLASSIC.with_rise(self.rise.presented())
        }
        /// The bottom limit the bespoke caption reader places a one-line block at, for `p`.
        fn limit(&self, p: Pos) -> f32 {
            caption_baseline(p, true, self.band(), 48.0)
        }
        fn limits(&self) -> [f32; 3] {
            [Pos::Low, Pos::Middle, Pos::High].map(|p| self.limit(p))
        }
    }

    fn same(got: [f32; 3], want: [f32; 3], what: &str) {
        assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.01), "{what}: {got:?} vs {want:?}");
    }

    /// The table, exact at the settled limit through the real reader, for each Position.
    #[test]
    fn the_rows_of_the_table_settle_exactly() {
        let mut rig = Rig::new();
        same(rig.limits(), [780.0, 620.0, 460.0], "HUD up, nothing open");
        rig.open(INFO);
        rig.settle();
        same(rig.limits(), [656.0, 620.0, 460.0], "Info (668)");
        rig.close(INFO);
        rig.settle();
        same(rig.limits(), [780.0, 620.0, 460.0], "the last close returns to P");
        rig.open(CHAPTERS);
        rig.settle();
        same(rig.limits(), [672.0, 620.0, 460.0], "Chapters (684)");
    }

    /// The image reader (Low's ceiling, no Position) and the bespoke reader land on the same limit, and
    /// the transport down ignores the rise.
    #[test]
    fn the_image_reader_stands_on_the_same_limit() {
        let mut rig = Rig::new();
        rig.open(INFO);
        rig.settle();
        let dialogue = Rect::new(300.0, 900.0, 1320.0, 135.0);
        let lift = hud_lift([dialogue].into_iter(), true, rig.band());
        assert!(close(1035.0 - lift, 656.0), "the lifted bottom stands on Info's limit: {lift}");
        assert_eq!(hud_lift([dialogue].into_iter(), false, rig.band()), 0.0, "the transport down moves nothing");
        assert_eq!(caption_baseline(Pos::Low, false, rig.band(), 48.0), caption_baseline(Pos::Low, false, HudBand::CLASSIC, 48.0));
    }

    /// Rise only: Info then Chapters on top holds, Chapters then Info rises 16, the close of one of two
    /// holds, the last close returns to P.
    #[test]
    fn while_a_section_stays_open_the_limit_only_rises() {
        let mut rig = Rig::new();
        rig.open(INFO);
        rig.settle();
        rig.open(CHAPTERS);
        rig.settle();
        same(rig.limits(), [656.0, 620.0, 460.0], "Info, then Chapters, holds");

        let mut rig = Rig::new();
        rig.open(CHAPTERS);
        rig.settle();
        same(rig.limits(), [672.0, 620.0, 460.0], "Chapters");
        rig.open(INFO);
        rig.settle();
        same(rig.limits(), [656.0, 620.0, 460.0], "Chapters, then Info, rises 16 px");
        rig.close(INFO);
        rig.settle();
        same(rig.limits(), [656.0, 620.0, 460.0], "closing one of two holds it");
        rig.close(CHAPTERS);
        rig.settle();
        same(rig.limits(), [780.0, 620.0, 460.0], "the last close returns to P");
        assert_eq!(rig.rise.presented(), [0.0; 3], "and exactly at rest");
    }

    /// A taller section first, then a shorter one: the shorter does not lower it.
    #[test]
    fn a_shorter_section_does_not_lower_a_held_limit() {
        let mut rig = Rig::new();
        rig.open(INFO);
        rig.settle();
        let tall = rig.rise.target();
        rig.open(CHAPTERS);
        rig.frame();
        assert_eq!(rig.rise.target(), tall);
    }

    /// With no section the limit never moves, whatever frames pass.
    #[test]
    fn with_no_section_the_limit_never_moves() {
        let mut rig = Rig::new();
        for _ in 0..120 {
            rig.frame();
            assert_eq!(rig.rise.presented(), [0.0; 3]);
            assert_eq!(rig.limits(), [780.0, 620.0, 460.0]);
        }
    }

    /// Where the caption's top would cross the safe area the limit stops at 54 + the caption height:
    /// the caption overlaps the section at its own size.
    #[test]
    fn a_caption_that_would_cross_the_safe_area_stops_at_it() {
        let block_h = 3.0 * 48.0 * 1.5;
        // a section 880 px tall: its limit would leave the block above the top of the screen
        let band = HudBand::CLASSIC.with_rise(HudBand::CLASSIC.rise_to_clear(200.0));
        let baseline = caption_baseline(Pos::Low, true, band, block_h);
        assert!(close(baseline, crate::ui::consts::SAFE.y + block_h), "the block stops under the safe top: {baseline}");
        assert!(baseline > 200.0, "it overlaps the section instead (captions draw above it)");
        // a block that fits is not clamped
        let free = caption_baseline(Pos::Low, true, HudBand::CLASSIC.with_rise(HudBand::CLASSIC.rise_to_clear(INFO_TOP)), 144.0);
        assert!(close(free, 656.0), "{free}");
        assert_eq!(crate::player::text_style::CAPTION_SAFE_TOP_Y, crate::ui::consts::SAFE.y, "media's copy of the safe top");
    }

    /// One delta for every image part below the limit; a part above it stays, and nothing is rescaled.
    #[test]
    fn every_part_below_the_limit_moves_by_one_delta_and_the_rest_stay() {
        let mut rig = Rig::new();
        rig.open(INFO);
        rig.settle();
        let band = rig.band();
        let (l1, l2) = (Rect::new(300.0, 900.0, 1320.0, 60.0), Rect::new(300.0, 960.0, 1320.0, 75.0));
        let above = Rect::new(300.0, 590.0, 1320.0, 60.0); // bottom 650: above Info's limit 656
        let sign = Rect::new(300.0, 100.0, 600.0, 50.0);
        let set = [l1, l2, above, sign];
        let moves: Vec<bool> = set.iter().map(|r| overhangs(*r, band)).collect();
        assert_eq!(moves, [true, true, false, false]);
        let lift = hud_lift(set.iter().copied(), true, band);
        assert!(close(lift, 1035.0 - 656.0), "{lift}");
        let moved = |r: Rect| if overhangs(r, band) { Rect::new(r.x, r.y - lift, r.w, r.h) } else { r };
        assert!(close(moved(l2).y - moved(l1).y, l2.y - l1.y));
        assert_eq!((moved(above), moved(sign)), (above, sign));
    }

    /// A cue changing mid-travel is laid out at the limit's current y.
    #[test]
    fn a_cue_that_changes_mid_travel_stands_on_the_current_limit() {
        let mut rig = Rig::new();
        rig.open(INFO);
        for _ in 0..6 {
            rig.frame();
        }
        let rise = rig.rise.presented()[LOW];
        assert!(rise > 0.0 && rise < 124.0, "mid-travel: {rise}");
        for lines in 1..=3 {
            let baseline = caption_baseline(Pos::Low, true, rig.band(), lines as f32 * 48.0);
            assert!(close(baseline, 780.0 - rise), "{lines} lines end at the limit's current y");
        }
    }

    /// The spring stops asking for frames when `nj_machine::motion::moving` says it has stopped, so it
    /// must land exactly on its target there: a limit left 0.1 px short would draw captions off 780
    /// with the section gone. The test reads the shared rest test, so loosening it cannot slip past.
    #[test]
    fn the_rise_lands_exactly_where_the_spring_stops_asking_for_frames() {
        for open in [true, false] {
            let mut rig = Rig::new();
            rig.open(INFO);
            rig.settle();
            if !open {
                rig.close(INFO);
            }
            for i in 0..300 {
                rig.frame();
                let (rise, target) = (rig.rise.presented()[LOW], rig.rise.target()[LOW]);
                assert!(
                    rise == target || nj_machine::motion::moving(rise, target, 0.0, 0.016_667),
                    "open={open} frame {i}: {rise} is {} px short of {target} and nothing asks for another frame",
                    (rise - target).abs()
                );
            }
        }
    }

    /// **A section held at appear 0 for several frames holds the limit for as many**: the container
    /// extends the hold while the host snapshot or the surface's text is pending; the limit waits with
    /// it and starts on the frame the section does.
    #[test]
    fn a_section_held_for_several_frames_holds_the_limit_with_it() {
        let mut rig = Rig::new();
        rig.open(INFO);
        let (mut held, mut started) = (0, None);
        for i in 0..40 {
            // the first five frames, the snapshot is still in flight: the hold is extended
            let in_flight = i < 5;
            let t = rig.tick();
            let mut ph = PresentHandle::of(&mut rig.present);
            for s in &mut rig.surfaces {
                s.motion.tick_gated(t, &mut ph, in_flight);
            }
            rig.player();
            let (appear, rise) = (rig.appear(INFO), rig.rise.presented()[LOW]);
            assert_eq!(appear > 0.0, rise > 0.0, "frame {i}: appear {appear}, rise {rise}");
            if appear == 0.0 { held += 1 } else if started.is_none() { started = Some(i) }
        }
        assert!(held >= 3, "the hold lasted {held} frames: the pin needs a multi-frame hold");
        assert!(started.is_some(), "and the section started once the snapshot was done");
    }

    /// The limit's first moving frame is the section's first appear frame, on open and on close, and it
    /// passes neither end (critically damped, on the section's own stiffness).
    #[test]
    fn the_limit_starts_with_the_section_and_never_overshoots() {
        for (bit, top) in [(INFO, INFO_TOP), (CHAPTERS, CHAPTERS_TOP)] {
            let mut rig = Rig::new();
            rig.open(bit);
            let target = HudBand::CLASSIC.rise_to_clear(top)[LOW];
            let (mut first_appear, mut first_rise) = (None, None);
            let mut last = 0.0;
            for i in 0..300 {
                rig.frame();
                let (appear, rise) = (rig.appear(bit), rig.rise.presented()[LOW]);
                if appear > 0.0 && first_appear.is_none() { first_appear = Some(i) }
                if rise > 0.0 && first_rise.is_none() { first_rise = Some(i) }
                assert!(rise >= last && rise <= target + 1e-3, "{bit} frame {i}: {rise} after {last}, target {target}");
                last = rise;
            }
            assert!(first_appear.is_some() && first_appear == first_rise, "{bit} open: appear {first_appear:?}, limit {first_rise:?}");
            assert_eq!(last, target, "settles exactly on the block");

            // the dismissal frame moves neither the section nor the limit; the next moves both
            rig.close(bit);
            assert_eq!((rig.appear(bit), rig.rise.presented()[LOW]), (1.0, target), "{bit}: the dismissal frame moves nothing");
            let (mut first_appear, mut first_rise) = (None, None);
            let mut last = target;
            for i in 0..300 {
                rig.frame();
                let (appear, rise) = (rig.appear(bit), rig.rise.presented()[LOW]);
                if appear < 1.0 && first_appear.is_none() { first_appear = Some(i) }
                if rise < target && first_rise.is_none() { first_rise = Some(i) }
                assert!(rise <= last && rise >= 0.0, "{bit} close frame {i}: {rise} after {last}");
                last = rise;
            }
            assert!(first_appear.is_some() && first_appear == first_rise, "{bit} close: appear {first_appear:?}, limit {first_rise:?}");
            assert_eq!(last, 0.0);
        }
    }

    /// The draw order follows what the sections actually draw: not the held frame, yes while open or
    /// fading out, not once faded.
    #[test]
    fn a_section_is_visible_only_while_it_draws_something() {
        let fold = |appear: f32, held: bool, open: bool| {
            OpenSections::fold([SectionSurface { bit: INFO, top: INFO_TOP, open, held, appear }])
        };
        assert!(!fold(0.0, true, true).visible, "held at appear 0");
        assert!(fold(0.01, false, true).visible);
        assert!(fold(0.5, false, false).visible, "fading out");
        assert!(!fold(0.0, false, false).visible, "faded");
        assert_eq!(fold(0.5, false, false).open, 0, "a closing section is not open");
        assert_eq!(fold(0.5, false, true).top, Some(INFO_TOP));
        assert_eq!(OpenSections::fold([]), OpenSections::default());
    }
}
