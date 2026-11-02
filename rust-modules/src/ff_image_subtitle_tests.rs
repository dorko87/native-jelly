//! Subtitles through the FFmpeg the app SHIPS — the component list and the ASS ingestion path are
//! the seams under test.
//!
//! The host suite has no FFmpeg to call, so most `ff` tests stay off `av_*`. These cannot: what
//! they grade is a property of `ci/build-ffmpeg.sh`'s configure line (zlib-compressed PGS tracks)
//! or of bytes only the real demuxer produces (an ASS header and packets that arrive NUL-terminated).
//! So they load the host build of that same FFmpeg (`HOST=1 ci/build-ffmpeg.sh`, staged into `pkg/`
//! by `make check-ffmpeg`), and the ASS ones also need the staged libass and fonts; all are
//! `#[ignore]`d everywhere else — an honest "not run" in `make check`, never a pass that executed
//! nothing. `bind_host_ffmpeg` is shared with `ff_late_audio_tests`, which runs a real demux.

use super::*;
use crate::player::{ass, ass_source};
use std::sync::Arc;

const TERMINATED_ASS_HEADER: &[u8] =
    include_bytes!("../../tests/fixtures/ass-terminated-header.bin");

struct AssFixture {
    fmt: *mut AVFormatContext,
    pkt: *mut AVPacket,
    // (stream index, kind, decoder) in FILE order — the demux loop's `sub_streams`
    subs: Vec<(c_int, SubKind, *mut AVCodecContext)>,
}

impl AssFixture {
    unsafe fn open() -> Self {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/ass-terminated-header.mkv");
        let path = CString::new(path.to_str().unwrap()).unwrap();
        let mut fmt: *mut AVFormatContext = std::ptr::null_mut();
        assert!(
            avformat_open_input(
                &mut fmt,
                path.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut()
            ) >= 0
        );
        let pkt = av_packet_alloc();
        assert!(!pkt.is_null());
        let mut subs: Vec<(c_int, SubKind, *mut AVCodecContext)> = Vec::new();
        for i in 0..(*fmt).nb_streams {
            let cp = stream_codecpar(*(*fmt).streams.add(i as usize));
            if (*cp).codec_type == AVMEDIA_TYPE_SUBTITLE {
                subs.push((i as c_int, sub_kind((*cp).codec_id), std::ptr::null_mut()));
            }
        }
        assert!(
            subs.len() == 2 && subs.iter().all(|(_, k, _)| *k == SubKind::Ass),
            "the fixture's two ASS tracks"
        );
        Self { fmt, pkt, subs }
    }

    unsafe fn sources(&self) -> u64 {
        begin_ass_sources("fixture:ass-header", self.fmt, (*self.fmt).streams, &self.subs)
    }

    /// The demux loop's ASS dispatch, timing included: the packet bytes go to
    /// `ass_source::push` untouched, so the terminators reach the store as FFmpeg returns them.
    unsafe fn read(&self, generation: u64) {
        while av_read_frame(self.fmt, self.pkt) >= 0 {
            let si = (*self.pkt).stream_index;
            if let Some(pos) = self.subs.iter().position(|(s, _, _)| *s == si) {
                assert!(
                    !(*self.pkt).data.is_null() && (*self.pkt).size > 0,
                    "a fixture subtitle packet must contain data"
                );
                let raw = std::slice::from_raw_parts((*self.pkt).data, (*self.pkt).size as usize);
                assert_eq!(raw.last() == Some(&0), pos == 0);
                let sst = *(*self.fmt).streams.add(si as usize);
                let start = pts_ns(self.pkt, sst);
                let dur = (*self.pkt).duration;
                let end = if dur > 0 {
                    start + av_rescale_q(dur, stream_time_base(sst), NS_TB)
                } else {
                    start + 4_000_000_000
                };
                ass_source::push(generation, pos as i32, start, end, raw);
            }
            av_packet_unref(self.pkt);
        }
    }
}

impl Drop for AssFixture {
    fn drop(&mut self) {
        unsafe {
            av_packet_free(&mut self.pkt);
            avformat_close_input(&mut self.fmt);
        }
    }
}

struct AssReset;
impl Drop for AssReset {
    fn drop(&mut self) {
        ass::clear();
        ass_source::reset();
    }
}

fn ass_frame(source: Arc<ass::Source>, now_ms: i64, visible: bool) -> Arc<ass::Frame> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(frame) = ass::request(source.clone(), now_ms, 320, 180, 320, 180) {
            if frame.error.is_some() || !frame.rects.is_empty() == visible {
                return frame;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ASS frame did not arrive at {now_ms} ms"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn assert_ass_caption(source: &Arc<ass::Source>, now_ms: i64) -> Arc<ass::Frame> {
    let frame = ass_frame(source.clone(), now_ms, true);
    assert_eq!(
        frame.error, None,
        "the terminated ASS header and packets must render their captions"
    );
    assert!(
        frame
            .rects
            .iter()
            .any(|r| r.rgba.chunks_exact(4).any(|p| p[3] != 0)),
        "caption pixels are visible"
    );
    frame
}

#[test]
#[ignore = "requires the bundled host FFmpeg, native libass and packaged fonts; run make check-ffmpeg"]
fn terminated_ass_header_and_packets_render_through_main_subtitle_ingestion() {
    let _g = nj_base::testlock::serial();
    let _reset = AssReset;
    let _ffmpeg = bind_host_ffmpeg();
    ass_source::reset();
    SHARED.playpos_ns.store(0, Ordering::Relaxed);
    unsafe {
        let d = AssFixture::open();
        let cp = stream_codecpar(*(*d.fmt).streams.add(d.subs[0].0 as usize));
        assert_eq!((*cp).extradata_size, 521);
        assert_eq!(
            std::slice::from_raw_parts((*cp).extradata, 521),
            TERMINATED_ASS_HEADER
        );
        let generation = d.sources();
        d.read(generation);
        let terminated = ass_source::selected(0).unwrap();
        let control = ass_source::selected(1).unwrap();
        let frame = assert_ass_caption(&terminated, 2000);
        let control_frame = assert_ass_caption(&control, 2000);
        assert_eq!(
            frame.rects, control_frame.rects,
            "a terminator does not change caption pixels"
        );
        for source in [&terminated, &control] {
            let ass::Content::Embedded { header, events, .. } = &source.content else {
                panic!()
            };
            assert_eq!(header.as_ref(), &TERMINATED_ASS_HEADER[..520]);
            assert_eq!(
                events[0].payload.as_ref(),
                b"0,0,Default,,0,0,0,,FIRST TEST CAPTION"
            );
            assert_eq!(
                events[1].payload.as_ref(),
                b"1,0,Default,,0,0,0,,SECOND TEST CAPTION"
            );
            assert_eq!(
                events
                    .iter()
                    .map(|e| (e.start_ms, e.duration_ms))
                    .collect::<Vec<_>>(),
                [(1730, 2000), (5000, 2000)]
            );
            assert_eq!(ass_frame(source.clone(), 4000, false).error, None);
            assert_ass_caption(source, 6000);
            assert_eq!(ass_frame(source.clone(), 8000, false).error, None);
            assert_ass_caption(source, 2000);
        }
        ass::clear();
        assert_ass_caption(&terminated, 2000);
        // A demux reopen of the same file must recognise the trimmed header and keep its events.
        let reopened_generation = d.sources();
        let reopened = ass_source::selected(0).unwrap();
        assert_ne!(reopened.id, terminated.id);
        assert_ass_caption(&reopened, 2000);
        ass_source::seek(reopened_generation);
        assert_ass_caption(&ass_source::selected(0).unwrap(), 6000);
    }
}

#[test]
#[ignore = "requires the bundled host FFmpeg, native libass and packaged fonts; run make check-ffmpeg"]
fn ass_packet_terminators_do_not_weaken_validation() {
    let _g = nj_base::testlock::serial();
    let _reset = AssReset;
    let _ffmpeg = bind_host_ffmpeg();
    unsafe {
        let d = AssFixture::open();
        SHARED.playpos_ns.store(0, Ordering::Relaxed);
        fn push(generation: u64, payload: &[u8]) {
            ass_source::push(generation, 0, 1_000_000_000, 3_000_000_000, payload);
        }
        for payload in [
            b"0,0,Default,,0,0,0,,bad\0caption\0".as_slice(),
            b"\0caption",
        ] {
            ass_source::reset();
            let generation = d.sources();
            push(generation, payload);
            push(generation, b"0,0,Default,,0,0,0,,VALID CAPTION");
            let source = ass_source::selected(0).unwrap();
            let ass::Content::Embedded { events, .. } = &source.content else {
                panic!()
            };
            assert_eq!(events.len(), 1, "the malformed packet must be dropped");
            assert_ass_caption(&source, 2000);
        }
        ass_source::reset();
        let generation = d.sources();
        for payload in [b"".as_slice(), b"\0", b"\0\0\0"] {
            push(generation, payload);
        }
        let oversized = vec![0; 256 * 1024 + 1];
        push(generation, &oversized);
        let source = ass_source::selected(0).unwrap();
        let ass::Content::Embedded { events, .. } = &source.content else {
            panic!()
        };
        assert!(
            events.is_empty(),
            "empty and oversized packets must be dropped"
        );
        push(generation, b"0,0,Default,,0,0,0,,MULTIPLE TERMINATORS\0\0\0");
        assert_ass_caption(&ass_source::selected(0).unwrap(), 2000);
    }
}

#[test]
#[ignore = "requires the bundled host FFmpeg, native libass and packaged fonts; run make check-ffmpeg"]
fn ass_header_terminators_do_not_weaken_validation() {
    let _g = nj_base::testlock::serial();
    let _reset = AssReset;
    let _ffmpeg = bind_host_ffmpeg();
    ass_source::reset();
    unsafe {
        let d = AssFixture::open();
        let cp = stream_codecpar(*(*d.fmt).streams.add(d.subs[0].0 as usize));
        // FFmpeg allocates zeroed AV_INPUT_BUFFER_PADDING_SIZE bytes after extradata.
        (*cp).extradata_size = 523;
        d.sources();
        let source = ass_source::selected(0).unwrap();
        let ass::Content::Embedded { header, .. } = &source.content else {
            panic!()
        };
        assert_eq!(header.as_ref(), &TERMINATED_ASS_HEADER[..520]);
        *(*cp).extradata.add(20) = 0;
        d.sources();
        assert_eq!(
            ass_frame(ass_source::selected(0).unwrap(), 2000, false).error,
            Some(ass::Fault::InvalidTrack)
        );
        std::ptr::write_bytes((*cp).extradata, 0, 523);
        d.sources();
        assert_eq!(
            ass_frame(ass_source::selected(0).unwrap(), 2000, false).error,
            Some(ass::Fault::InvalidTrack)
        );
        (*cp).extradata_size = 0;
        d.sources();
        assert_eq!(
            ass_frame(ass_source::selected(0).unwrap(), 2000, false).error,
            Some(ass::Fault::InvalidTrack)
        );
        let allocated = (*cp).extradata;
        (*cp).extradata = std::ptr::null_mut();
        (*cp).extradata_size = 521;
        d.sources();
        (*cp).extradata = allocated;
        assert_eq!(
            ass_frame(ass_source::selected(0).unwrap(), 2000, false).error,
            Some(ass::Fault::InvalidTrack)
        );
        (*cp).extradata_size = ass_source::MAX_HEADER_BYTES as i32 + 1;
        d.sources();
        assert!(
            ass_source::selected(0).is_none(),
            "the raw size bound runs before copying or trimming"
        );
    }
}

/// Where the host FFmpeg was staged: `NJ_FFMPEG_DIR` (what `make check-ffmpeg` passes), else the
/// checkout's own `pkg/`.
fn host_ffmpeg_dir() -> std::path::PathBuf {
    std::env::var_os("NJ_FFMPEG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pkg"))
}

#[test]
#[ignore = "requires the bundled host FFmpeg; run make check-ffmpeg"]
fn host_ffmpeg_binding_restores_the_abi_gate() {
    let _g = nj_base::testlock::serial();
    assert!(!abi_ok(), "host tests must leave the ABI gate closed");
    {
        let _ffmpeg = bind_host_ffmpeg();
        assert!(abi_ok());
        {
            let _nested = bind_host_ffmpeg();
            assert!(abi_ok());
        }
        assert!(abi_ok(), "an inner binding must preserve the outer binding");
    }
    assert!(!abi_ok(), "the next host test must start with a closed ABI gate");
}

pub(super) struct HostFfmpeg {
    previous_abi_ok: bool,
}

impl Drop for HostFfmpeg {
    fn drop(&mut self) {
        ABI_OK.store(self.previous_abi_ok, Ordering::Relaxed);
    }
}

/// Bind the three libraries exactly as `load_libraries` does — dependency order, one directory —
/// and hold the build to the ABI table this file's offsets were checked against, then open the
/// ABI gate. Keep the guard until all readers stop so the next test gets the previous ABI state.
#[must_use]
pub(super) fn bind_host_ffmpeg() -> HostFfmpeg {
    nj_base::testlock::assert_held("bind_host_ffmpeg");
    let dir = host_ffmpeg_dir();
    for (what, verdict) in [
        ("avutil", avutil::load(Some(&dir))),
        ("avcodec", avcodec::load(Some(&dir))),
        ("avformat", avformat::load(Some(&dir))),
    ] {
        assert!(
            matches!(verdict, nj_base::dynlib::Loaded::Ok(_)),
            "{what}: no host FFmpeg in {} — run `make check-ffmpeg`",
            dir.display()
        );
    }
    let majors = unsafe {
        (
            avformat_version() >> 16,
            avcodec_version() >> 16,
            avutil_version() >> 16,
        )
    };
    assert_eq!(
        majors,
        (63, 63, 61),
        "the staged FFmpeg is not the one ff.rs is built for"
    );
    HostFfmpeg {
        previous_abi_ok: ABI_OK.swap(true, Ordering::Relaxed),
    }
}

/// **A subtitle track muxed by mkvmerge is zlib-compressed, and the bundled FFmpeg must undo it.**
///
/// mkvmerge's default for `S_HDMV/PGS` and `S_VOBSUB` tracks is Matroska ContentCompression with
/// zlib, so that is what nearly every disc remux in the wild carries. libavformat's matroska
/// demuxer only inflates it when FFmpeg was configured WITH zlib; `--disable-autodetect` had
/// quietly dropped it, and without it the demuxer logs "Unsupported encoding type" and hands the
/// decoder the still-compressed bytes (`78 da …`). pgssubdec parses those as segments, finds no
/// display set, and returns got=0 on every packet — playback is perfect and not one `image cue`
/// is ever produced, which is exactly what the television showed on two different libraries.
///
/// The fix is in the demuxer, so it is graded for every consumer of a compressed track, not only
/// the one that was reported. The fixture's five tracks, in file order:
///   0. PGS, compressed as mkvmerge writes it by default;
///   1. the same PGS with `--compression 0:none` — the shape ffmpeg's own muxer produces, and the
///      only one the synthetic harness media ever exercised (the control);
///   2. VobSub transcoded from that PGS, compressed (mkvmerge's default for VobSub too);
///   3. the same VobSub uncompressed (its control);
///   4. an SRT forced to `--compression 0:zlib` — text tracks take the demux loop's payload path,
///      not a decoder, so for them the proof is the packet bytes themselves.
///
/// Regenerate from `tests/fixtures/`:
/// `python3 -c 'import make_fixtures as m, pathlib; m.pgs_build(pathlib.Path("s.sup"), 9)'`
/// (one cue at 0 s, cleared at 8 s); `ffmpeg -f sup -i s.sup -map 0 -c:s dvdsub -s 1920x1080 v.mkv`;
/// an SRT `t.srt` holding one cue `ZLIB TEXT CUE`; then `mkvmerge -o mkv_zlib_subs.mkv --no-date
/// --disable-track-statistics-tags s.sup --compression 0:none s.sup v.mkv --compression 0:none
/// v.mkv --compression 0:zlib t.srt`.
///
/// The image tracks go through the app's own decode path — `open_sub_decoder`, `decode_bitmap_cue`,
/// `rect_to_indexed`, `sub_canvas`, `push_subtitle_bitmap` — keyed by file-order ordinal as the demux
/// loop keys them.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg — run by `make check-ffmpeg`"]
fn mkvmerge_zlib_subtitle_tracks_decode_like_uncompressed_ones() {
    let _g = nj_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/mkv_zlib_subs.mkv")
        .canonicalize()
        .expect("fixture present");
    let url = CString::new(path.to_str().unwrap()).unwrap();

    SHARED.sub_bitmaps.lock().unwrap().clear();
    SHARED.playpos_ns.store(0, Ordering::Relaxed);
    let mut text_payloads: Vec<Vec<u8>> = Vec::new();
    unsafe {
        let mut fmt: *mut AVFormatContext = std::ptr::null_mut();
        assert!(
            avformat_open_input(
                &mut fmt,
                url.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut()
            ) >= 0,
            "open the fixture"
        );
        let streams = (*fmt).streams;
        // (stream index, kind, decoder) in FILE order — the demux loop's `sub_streams`
        let mut subs: Vec<(c_int, SubKind, *mut AVCodecContext)> = Vec::new();
        for i in 0..(*fmt).nb_streams {
            let cp = stream_codecpar(*streams.add(i as usize));
            if (*cp).codec_type == AVMEDIA_TYPE_SUBTITLE {
                let k = sub_kind((*cp).codec_id);
                let dec = if k == SubKind::Bitmap {
                    let d = open_sub_decoder(cp);
                    assert!(
                        !d.is_null(),
                        "the bundled build decodes subtitle stream #{i}"
                    );
                    d
                } else {
                    std::ptr::null_mut()
                };
                subs.push((i as c_int, k, dec));
            }
        }
        let kinds: Vec<bool> = subs.iter().map(|(_, k, _)| *k == SubKind::Bitmap).collect();
        assert_eq!(
            kinds,
            [true, true, true, true, false],
            "the fixture's five tracks"
        );

        let pkt = av_packet_alloc();
        while av_read_frame(fmt, pkt) >= 0 {
            let si = (*pkt).stream_index;
            if let Some(pos) = subs.iter().position(|(s, _, _)| *s == si) {
                let (_, kind, dec) = subs[pos];
                if kind == SubKind::Bitmap {
                    decode_bitmap_cue(dec, pkt, pos as c_int, *streams.add(si as usize));
                } else {
                    let n = (*pkt).size.max(0) as usize;
                    text_payloads.push(std::slice::from_raw_parts((*pkt).data, n).to_vec());
                }
            }
            av_packet_unref(pkt);
        }
        let mut p = pkt;
        av_packet_free(&mut p);
        for (_, _, dec) in subs.iter_mut() {
            if !dec.is_null() {
                avcodec_free_context(dec);
            }
        }
        avformat_close_input(&mut fmt);
    }

    let v = SHARED.sub_bitmaps.lock().unwrap();
    let cues = |track: i32| -> Vec<(i64, i64, usize)> {
        v.iter()
            .filter(|c| c.track == track)
            .map(|c| (c.start_ns, c.end_ns, c.rects.len()))
            .collect()
    };
    let canvas = |track: i32| v.iter().find(|c| c.track == track).map(|c| (c.cw, c.ch));
    let (pgs_zlib, pgs_plain, vob_zlib, vob_plain) = (cues(0), cues(1), cues(2), cues(3));
    let pgs_canvas = canvas(0);
    drop(v);
    SHARED.sub_bitmaps.lock().unwrap().clear();

    // the controls: uncompressed tracks decode, so the fixture and the decode path are sound
    assert_eq!(
        pgs_plain.len(),
        1,
        "uncompressed PGS: its one cue, got {pgs_plain:?}"
    );
    assert_eq!(
        vob_plain.len(),
        1,
        "uncompressed VobSub: its one cue, got {vob_plain:?}"
    );
    // the regression: the same display sets, zlib-compressed the way mkvmerge writes them
    assert_eq!(
        pgs_zlib, pgs_plain,
        "the zlib-compressed PGS track must decode to the same cues as the uncompressed one"
    );
    assert_eq!(
        pgs_canvas,
        Some((1920, 1080)),
        "the PGS cue carries its authoring canvas"
    );
    assert_eq!(
        vob_zlib, vob_plain,
        "the zlib-compressed VobSub track must decode to the same cues as the uncompressed one"
    );
    // ...and a compressed TEXT track reaches the payload path as text, not as `78 da …`
    assert_eq!(text_payloads.len(), 1, "the SRT track's one packet");
    assert_eq!(
        String::from_utf8_lossy(&text_payloads[0]),
        "ZLIB TEXT CUE",
        "the zlib-compressed SRT packet must arrive inflated"
    );
}

/// A muxer can copy GBK bytes into a nominally UTF-8 SubRip track. Exercise the
/// bundled demuxer's actual packets and language metadata, without a server.
#[test]
#[ignore = "requires the bundled host FFmpeg; run make check-ffmpeg"]
fn legacy_subrip_packets_decode_using_the_container_language() {
    let _guard = nj_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/gbk-subrip.mkv").canonicalize().unwrap();
    let url = CString::new(path.to_str().unwrap()).unwrap();
    unsafe {
        let mut fmt = std::ptr::null_mut();
        assert!(avformat_open_input(&mut fmt, url.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) >= 0);
        let streams = (*fmt).streams;
        assert_eq!((*fmt).nb_streams, 1);
        assert!(sub_kind((*stream_codecpar(*streams)).codec_id) == SubKind::Plain);
        let language = stream_tag(*streams, c"language");
        assert_eq!(language.as_deref(), Some("chi"));
        let charset = crate::player::sub_charset::TrackCharset::new(language.as_deref());
        let pkt = av_packet_alloc();
        assert!(!pkt.is_null());
        let mut cues = Vec::new();
        while av_read_frame(fmt, pkt) >= 0 {
            let raw = std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize);
            cues.push(crate::player::sub_charset::subtitle_text(raw, &charset).into_owned());
            av_packet_unref(pkt);
        }
        let mut pkt = pkt;
        av_packet_free(&mut pkt);
        avformat_close_input(&mut fmt);
        assert_eq!(cues, ["你好，世界！这是中文字幕。", "简体中文测试"]);
    }
}
