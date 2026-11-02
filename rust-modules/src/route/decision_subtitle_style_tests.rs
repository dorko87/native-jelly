//! The Size / Position / HDR overlay brightness persistence path: publish the live atomic first,
//! persist second, and never republish what the worker learned. Also the HDR overlay dim rule.
use super::*;
use crate::catalog::session::{HdrOverlayBrightness, SubtitlePosition, SubtitleSize};
use std::sync::mpsc;

/// Restore the two atomics the tests move, whatever the outcome.
struct Restore(SubtitleSize, SubtitlePosition);
impl Drop for Restore {
    fn drop(&mut self) {
        restore_subtitle_size(self.0);
        restore_subtitle_position(self.1);
    }
}

fn recv(rx: &mpsc::Receiver<bool>) -> bool {
    rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the persist-only write answered")
}

/// **A pick is live at once and durable later.** The atomic the caption draw reads is the new
/// value the moment `select_*` returns — before the worker has done anything — and the reply then
/// reports the durable write, which the next load reads back.
#[test]
fn select_publishes_the_live_value_before_the_write_and_persists_it() {
    let _g = nj_base::testlock::serial();
    let session = crate::catalog::session::TempSession::new("select-style");
    let _restore = Restore(subtitle_size(), subtitle_position());
    restore_subtitle_size(SubtitleSize::Medium);
    restore_subtitle_position(SubtitlePosition::Low);

    let (tx, rx) = mpsc::channel();
    select_subtitle_size(SubtitleSize::Large, Some(tx));
    assert_eq!(subtitle_size(), SubtitleSize::Large, "published synchronously, ahead of the worker");
    assert!(recv(&rx), "a writable session is durable");
    assert_eq!(crate::catalog::session::load().subtitle_size, SubtitleSize::Large);

    let (tx, rx) = mpsc::channel();
    select_subtitle_position(SubtitlePosition::High, Some(tx));
    assert_eq!(subtitle_position(), SubtitlePosition::High);
    assert!(recv(&rx));
    assert_eq!(crate::catalog::session::load().subtitle_position, SubtitlePosition::High);
    drop(session);
}

/// **The write never republishes.** A newer live pick made while an older write is in flight must
/// survive the older write's completion: the worker touches the durable file only.
#[test]
fn select_never_republishes_when_the_write_completes() {
    let _g = nj_base::testlock::serial();
    let _session = crate::catalog::session::TempSession::new("select-no-republish");
    let _restore = Restore(subtitle_size(), subtitle_position());
    restore_subtitle_size(SubtitleSize::Medium);

    let (tx, rx) = mpsc::channel();
    select_subtitle_size(SubtitleSize::Large, Some(tx));
    restore_subtitle_size(SubtitleSize::Small); // a newer pick, made before the worker answers
    assert!(recv(&rx));
    assert_eq!(subtitle_size(), SubtitleSize::Small, "the completed write left the live value alone");
}

/// **A failed write claims nothing**: the reply says `false` and the live value the viewer picked
/// stays for this session (it is the durable write that failed, not the pick).
#[test]
fn select_reports_a_failed_write_and_keeps_the_live_value() {
    let _g = nj_base::testlock::serial();
    let session = crate::catalog::session::TempSession::new("select-failed");
    let _restore = Restore(subtitle_size(), subtitle_position());
    restore_subtitle_size(SubtitleSize::Medium);
    // the session file's parent is a regular file, so no write can land
    let dir = std::env::temp_dir().join(format!("nativejelly-select-blocked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("blocker"), b"x").unwrap();
    crate::catalog::session::redirect_for_test(Some(dir.join("blocker").join("auth.json")));

    let (tx, rx) = mpsc::channel();
    select_subtitle_size(SubtitleSize::ExtraLarge, Some(tx));
    assert!(!recv(&rx), "an unwritable session is not durable");
    assert_eq!(subtitle_size(), SubtitleSize::ExtraLarge, "the live pick stands");
    crate::catalog::session::redirect_for_test(None);
    let _ = std::fs::remove_dir_all(&dir);
    drop(session);
}

/// **The HDR overlay brightness pick takes the same optimistic door** (#741): live at once, durable
/// later, and the durable value is what the next load reads.
#[test]
fn select_hdr_overlay_brightness_publishes_first_and_persists() {
    let _g = nj_base::testlock::serial();
    let session = crate::catalog::session::TempSession::new("select-hdr-overlay");
    struct Put(HdrOverlayBrightness);
    impl Drop for Put {
        fn drop(&mut self) {
            restore_hdr_overlay_brightness(self.0);
        }
    }
    let _restore = Put(hdr_overlay_brightness());
    restore_hdr_overlay_brightness(HdrOverlayBrightness::ThreeQuarters);

    let (tx, rx) = mpsc::channel();
    select_hdr_overlay_brightness(HdrOverlayBrightness::Half, Some(tx));
    assert_eq!(hdr_overlay_brightness(), HdrOverlayBrightness::Half, "published synchronously, ahead of the worker");
    assert!(recv(&rx), "a writable session is durable");
    assert_eq!(crate::catalog::session::load().hdr_overlay_brightness, HdrOverlayBrightness::Half);

    let (tx, rx) = mpsc::channel();
    select_hdr_overlay_brightness(HdrOverlayBrightness::Full, Some(tx));
    assert!(recv(&rx));
    assert_eq!(crate::catalog::session::load().hdr_overlay_brightness, HdrOverlayBrightness::Full,
        "a chosen 100% is written, not skipped as the default");
    drop(session);
}

/// The overlay dim rule: nothing changes for SDR or at 100%; HDR at 75% / 50% scales by exactly
/// that; a server re-encode is an SDR picture whatever the source was, a remux keeps it HDR.
#[test]
fn overlay_gain_is_one_for_sdr_and_full_and_the_rung_for_hdr() {
    use HdrOverlayBrightness::{Full, Half, ThreeQuarters};
    for pick in [Full, ThreeQuarters, Half] {
        assert_eq!(overlay_gain_for(false, false, false, pick), 1.0, "SDR direct play, {pick:?}");
        assert_eq!(overlay_gain_for(false, true, true, pick), 1.0, "SDR remux, {pick:?}");
    }
    assert_eq!(overlay_gain_for(true, false, false, Full), 1.0);
    assert_eq!(overlay_gain_for(true, false, false, ThreeQuarters), 0.75);
    assert_eq!(overlay_gain_for(true, false, false, Half), 0.5);
    assert_eq!(overlay_gain_for(true, true, true, Half), 0.5, "a remux keeps the HDR stream");
    assert_eq!(overlay_gain_for(true, true, false, Half), 1.0, "a re-encode is tone-mapped to SDR");
}

#[test]
fn the_default_pick_dims_an_hdr_picture_to_75_percent_and_never_an_sdr_one() {
    assert_eq!(HdrOverlayBrightness::default(), HdrOverlayBrightness::ThreeQuarters);
    assert_eq!(HdrOverlayBrightness::from_index(99), HdrOverlayBrightness::ThreeQuarters);
    assert_eq!(overlay_gain_for(true, false, false, HdrOverlayBrightness::default()), 0.75);
    assert_eq!(overlay_gain_for(false, false, false, HdrOverlayBrightness::default()), 1.0);
    assert_eq!(overlay_gain_for(true, false, false, HdrOverlayBrightness::Full), 1.0, "a chosen 100% stays off");
}

/// [`overlay_gain`] reads the live route the way a Jellyfin playback leaves it: a direct play
/// (no encoder) and a conversion whose video lane is copied (`Lane::copied` -> `contract.remux`)
/// both show the source's HDR, a conversion that re-encodes the video does not.
#[test]
fn overlay_gain_reads_the_installed_route_and_the_live_pick() {
    let _g = nj_base::testlock::serial();
    struct Put(HdrOverlayBrightness);
    impl Drop for Put {
        fn drop(&mut self) {
            restore_hdr_overlay_brightness(self.0);
        }
    }
    let _restore = Put(hdr_overlay_brightness());
    restore_hdr_overlay_brightness(HdrOverlayBrightness::Half);

    let mut ps = PlaybackSession::IDLE;
    assert_eq!(overlay_gain(&ps, true), 0.5, "direct play of an HDR source");
    assert_eq!(overlay_gain(&ps, false), 1.0, "direct play of an SDR source");
    install_transcode_for_test(&mut ps, true, false);
    assert_eq!(overlay_gain(&ps, true), 0.5, "the video lane is copied");
    install_transcode_for_test(&mut ps, false, true);
    assert_eq!(overlay_gain(&ps, true), 1.0, "the video lane is re-encoded");
    restore_hdr_overlay_brightness(HdrOverlayBrightness::Full);
    assert_eq!(overlay_gain(&PlaybackSession::IDLE, true), 1.0, "100% never dims");
}
