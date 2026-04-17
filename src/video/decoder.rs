use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use crate::vprintln;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use anyhow::{anyhow, Result};

// Speed table (must match ui::control_bar::SPEEDS).
const SPEEDS: [f32; 5] = [1.0, 2.0 / 3.0, 0.5, 1.0 / 3.0, 0.25];

/// Pixel format carried by a `VideoFrame`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VideoFormat {
    /// Tightly packed BGRA, `width × height × 4` bytes.
    Bgra,
    /// Raw NV12 from the decoder.
    /// `stride`    – bytes per row (≥ width, due to decoder alignment padding).
    /// `uv_offset` – byte offset from `data[0]` to the first UV byte.
    ///               This is `coded_height × stride`, which may be larger than
    ///               `display_height × stride` when the codec pads height to a
    ///               block boundary (e.g. H.264 pads 1080 → 1088).
    Nv12 { stride: usize, uv_offset: usize },
}

/// A decoded video frame.
#[derive(Clone)]
pub struct VideoFrame {
    pub data:   Vec<u8>,
    pub format: VideoFormat,
}

/// Opens a video file and decodes it on a background thread via Windows Media Foundation.
/// The latest decoded frame is available via `take_frame()`.
pub struct VideoDecoder {
    latest: Arc<Mutex<Option<VideoFrame>>>,
    /// Current presentation timestamp in microseconds, updated by the decode thread.
    pub current_pts_us: Arc<AtomicU64>,
    /// Duration of the video in microseconds (0 if not known).
    pub duration_us: u64,
    /// True when the decoder outputs NV12 frames (GPU colour conversion).
    /// False when it outputs BGRA (already converted on the CPU).
    pub is_nv12: bool,
    /// When `true` the decode thread pauses between frames.
    pub paused: Arc<AtomicBool>,
    /// Index into `SPEEDS` (0 = 1×, 4 = ¼×).
    pub speed_index: Arc<AtomicU32>,
    /// Loop state: 0 = off, 1 = start set, 2 = active (start + end set).
    pub loop_state: Arc<AtomicU8>,
    pub loop_start_us: Arc<AtomicU64>,
    pub loop_end_us: Arc<AtomicU64>,
    /// Write a microsecond target here to request a seek.
    pub seek_request: Arc<Mutex<Option<u64>>>,
    /// Audio seek target in microseconds; u64::MAX means no seek pending.
    pub audio_seek: Arc<AtomicU64>,
    /// Incremented on every seek so the cpal callback flushes stale buffered audio.
    pub audio_flush_gen: Arc<AtomicU64>,
    pub width: u32,
    pub height: u32,
    /// Set to `true` to signal both decode threads to exit.
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    audio_thread: Option<thread::JoinHandle<()>>,
    _audio_player: Option<crate::audio::AudioPlayer>,
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock the audio channel if the audio thread is sleeping on a full send.
        // (The video thread checks stop at the top of every loop iteration.)
        if let Some(h) = self.thread.take()       { let _ = h.join(); }
        if let Some(h) = self.audio_thread.take() { let _ = h.join(); }
    }
}

impl VideoDecoder {
    pub fn open(source: String) -> Result<Self> {

        // init message: (width, height, duration_us, is_nv12)
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(u32, u32, u64, bool)>>();
        let latest: Arc<Mutex<Option<VideoFrame>>> = Arc::new(Mutex::new(None));
        let latest_clone = Arc::clone(&latest);
        let current_pts_us  = Arc::new(AtomicU64::new(0));
        let paused          = Arc::new(AtomicBool::new(false));
        let speed_index     = Arc::new(AtomicU32::new(0));
        let loop_state      = Arc::new(AtomicU8::new(0));
        let loop_start_us   = Arc::new(AtomicU64::new(0));
        let loop_end_us     = Arc::new(AtomicU64::new(0));
        let seek_request    = Arc::new(Mutex::new(None::<u64>));
        let audio_seek      = Arc::new(AtomicU64::new(u64::MAX));
        let audio_flush_gen = Arc::new(AtomicU64::new(0));
        let audio_started   = Arc::new(AtomicBool::new(false));
        let stop            = Arc::new(AtomicBool::new(false));

        // Bounded channel: ~2 seconds of float32 stereo audio at 48 kHz.
        let (audio_tx, audio_rx) =
            std::sync::mpsc::sync_channel::<f32>(48_000 * 2 * 2);
        let (audio_fmt_tx, audio_fmt_rx) =
            std::sync::mpsc::channel::<Option<(u32, u16)>>();

        let pts_c        = Arc::clone(&current_pts_us);
        let pau_c        = Arc::clone(&paused);
        let spd_c        = Arc::clone(&speed_index);
        let lps_c        = Arc::clone(&loop_state);
        let lps_start_c  = Arc::clone(&loop_start_us);
        let lps_end_c    = Arc::clone(&loop_end_us);
        let seek_c       = Arc::clone(&seek_request);
        let vid_audio_seek    = Arc::clone(&audio_seek);
        let vid_audio_flush   = Arc::clone(&audio_flush_gen);
        let vid_audio_started = Arc::clone(&audio_started);

        // Clone source string before moving it into the video thread.
        let audio_source    = source.clone();
        let audio_paused    = Arc::clone(&paused);
        let audio_speed     = Arc::clone(&speed_index);
        let audio_seek_c    = Arc::clone(&audio_seek);
        let audio_flush_c   = Arc::clone(&audio_flush_gen);
        let stop_video      = Arc::clone(&stop);
        let stop_audio      = Arc::clone(&stop);

        let handle = thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                decode_thread(
                    source, init_tx, latest_clone,
                    pts_c, pau_c, spd_c,
                    lps_c, lps_start_c, lps_end_c, seek_c,
                    vid_audio_seek, vid_audio_flush, vid_audio_started, stop_video,
                )
            })?;

        let audio_handle = thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || {
                audio_decode_thread(
                    audio_source, audio_fmt_tx, audio_tx,
                    audio_paused, audio_speed, audio_seek_c, audio_flush_c, stop_audio,
                )
            })?;

        let (width, height, duration_us, is_nv12) = init_rx
            .recv()
            .map_err(|_| anyhow!("Decoder thread exited before sending init result"))??;

        // Wait up to 5 s for the audio thread to report its format (or None = no audio).
        let audio_player = audio_fmt_rx
            .recv_timeout(Duration::from_secs(5))
            .ok()
            .flatten()
            .and_then(|(sr, ch)| {
                crate::audio::AudioPlayer::start(
                    audio_rx, sr, ch,
                    Arc::clone(&paused),
                    Arc::clone(&speed_index),
                    Arc::clone(&audio_flush_gen),
                    Arc::clone(&audio_started),
                )
            });

        Ok(Self {
            latest,
            current_pts_us,
            duration_us,
            is_nv12,
            paused,
            speed_index,
            loop_state,
            loop_start_us,
            loop_end_us,
            seek_request,
            audio_seek,
            audio_flush_gen,
            width,
            height,
            stop,
            thread: Some(handle),
            audio_thread: Some(audio_handle),
            _audio_player: audio_player,
        })
    }

    /// Take the latest decoded frame, leaving the slot empty.
    /// Returns `None` if no new frame has arrived since the last call.
    pub fn take_frame(&self) -> Option<VideoFrame> {
        self.latest.lock().unwrap().take()
    }
}

// ── background decode thread ───────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn decode_thread(
    source: String,
    init_tx: std::sync::mpsc::Sender<Result<(u32, u32, u64, bool)>>,
    latest: Arc<Mutex<Option<VideoFrame>>>,
    current_pts_us:  Arc<AtomicU64>,
    paused:          Arc<AtomicBool>,
    speed_index:     Arc<AtomicU32>,
    loop_state:      Arc<AtomicU8>,
    loop_start_us:   Arc<AtomicU64>,
    loop_end_us:     Arc<AtomicU64>,
    seek_request:    Arc<Mutex<Option<u64>>>,
    audio_seek:      Arc<AtomicU64>,
    audio_flush_gen: Arc<AtomicU64>,
    audio_started:   Arc<AtomicBool>,
    stop:            Arc<AtomicBool>,
) {
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::*;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        const MF_VER: u32 = 0x0002_0070;
        // 0 = MFSTARTUP_FULL — initialises networking so http(s):// URLs work.
        if let Err(e) = MFStartup(MF_VER, 0) {
            let _ = init_tx.send(Err(anyhow!("MFStartup failed: {e}")));
            CoUninitialize();
            return;
        }

        match open_and_decode(
            source, init_tx, &latest,
            &current_pts_us, &paused, &speed_index,
            &loop_state, &loop_start_us, &loop_end_us, &seek_request,
            &audio_seek, &audio_flush_gen, &audio_started, &stop,
        ) {
            Ok(()) => {}
            Err(e) => eprintln!("Video decoder error: {e}"),
        }

        let _ = MFShutdown();
        CoUninitialize();
    }
}

// ── D3D11 device manager for hardware-accelerated decode ──────────────────

/// Create a D3D11 device (with video support) and wrap it in an
/// `IMFDXGIDeviceManager` so the MF source reader can use DXVA2/D3D11VA.
fn create_dxgi_device_manager()
    -> Result<windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager>
{
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, ID3D11Device,
        ID3D11Multithread,
    };
    use windows::Win32::Media::MediaFoundation::{
        IMFDXGIDeviceManager, MFCreateDXGIDeviceManager,
    };
    use windows::core::Interface as _;

    unsafe {
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        ).map_err(|e| anyhow!("D3D11CreateDevice: {e}"))?;
        let device = device.unwrap();

        // Required: enable multithreaded protection on the device before
        // passing it to MF, which calls it from its own internal threads.
        let mt: ID3D11Multithread = device.cast()
            .map_err(|e| anyhow!("ID3D11Multithread: {e}"))?;
        let _ = mt.SetMultithreadProtected(true);

        let mut reset_token: u32 = 0;
        let mut mgr: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut reset_token, &mut mgr)
            .map_err(|e| anyhow!("MFCreateDXGIDeviceManager: {e}"))?;
        let mgr = mgr.unwrap();

        mgr.ResetDevice(&device, reset_token)
            .map_err(|e| anyhow!("IMFDXGIDeviceManager::ResetDevice: {e}"))?;

        Ok(mgr)
    }
}

// ── what pixel format the source reader is delivering ─────────────────────

#[derive(Copy, Clone, PartialEq, Debug)]
enum DecodeFmt {
    Bgra,
    Nv12,
    Yuy2,
}

// ── seek helper ────────────────────────────────────────────────────────────

/// Seek the reader to `target_us` microseconds.
unsafe fn seek_reader_to(
    reader: &windows::Win32::Media::MediaFoundation::IMFSourceReader,
    target_us: u64,
) {
    use std::mem::ManuallyDrop;
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
    };
    use windows::Win32::System::Variant::VT_I8;

    let target_100ns = (target_us * 10) as i64;
    let pv = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_I8,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { hVal: target_100ns },
            }),
        },
    };

    let guid_null = windows::core::GUID::default();
    unsafe {
        if let Err(e) = reader.SetCurrentPosition(&guid_null, &pv) {
            eprintln!("Seek failed: {e}");
        }
    }
}

// ── duration query ─────────────────────────────────────────────────────────

unsafe fn query_duration_us(
    reader: &windows::Win32::Media::MediaFoundation::IMFSourceReader,
) -> u64 {
    use windows::Win32::Media::MediaFoundation::MF_PD_DURATION;
    use windows::Win32::System::Variant::VT_UI8;

    // MF_SOURCE_READER_MEDIASOURCE = 0xFFFF_FFFF
    let Ok(pv) = (unsafe { reader.GetPresentationAttribute(0xFFFF_FFFF, &MF_PD_DURATION) }) else {
        return 0;
    };

    unsafe {
        let inner = &pv.Anonymous.Anonymous;
        if inner.vt == VT_UI8 {
            inner.Anonymous.uhVal / 10 // 100-ns → µs
        } else {
            0
        }
    }
}

// ── main decode function ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn open_and_decode(
    source: String,
    init_tx: std::sync::mpsc::Sender<Result<(u32, u32, u64, bool)>>,
    latest: &Arc<Mutex<Option<VideoFrame>>>,
    current_pts_us:  &Arc<AtomicU64>,
    paused:          &Arc<AtomicBool>,
    speed_index:     &Arc<AtomicU32>,
    loop_state:      &Arc<AtomicU8>,
    loop_start_us:   &Arc<AtomicU64>,
    loop_end_us:     &Arc<AtomicU64>,
    seek_request:    &Arc<Mutex<Option<u64>>>,
    audio_seek:      &Arc<AtomicU64>,
    audio_flush_gen: &Arc<AtomicU64>,
    audio_started:   &Arc<AtomicBool>,
    stop:            &Arc<AtomicBool>,
) -> Result<()> {
    use windows::Win32::Media::MediaFoundation::*;
    use windows::core::PCWSTR;

    const FIRST_VIDEO: u32 = 0xFFFF_FFFC;
    const ALL_STREAMS: u32 = 0xFFFF_FFFE;

    // MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS: use hardware MFTs (encoders/decoders).
    // NOTE: hardware VIDEO DECODE (DXVA2/D3D11VA) also requires MF_SOURCE_READER_D3D_MANAGER;
    // without a D3D device manager MF silently falls back to software decode.
    const MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS: windows::core::GUID = windows::core::GUID {
        data1: 0xa9cbbea3, data2: 0xd63a, data3: 0x4440,
        data4: [0x8d, 0x47, 0x46, 0x8d, 0x4e, 0x7e, 0xa6, 0xd7],
    };

    // Wide-string URL/path — built once, reused on each loop iteration.
    let wide: Vec<u16> = if source.starts_with("http://") || source.starts_with("https://") {
        source.encode_utf16().chain([0u16]).collect()
    } else {
        use std::os::windows::ffi::OsStrExt;
        std::path::Path::new(&source).as_os_str().encode_wide().chain([0u16]).collect()
    };
    let url = PCWSTR(wide.as_ptr());

    /// Open a fresh IMFSourceReader for the given URL, configured for video decode.
    /// Returns (reader, format, width, height, duration_us).
    fn open_reader(
        url: PCWSTR,
        source: &str,
        mf_hw_guid: &windows::core::GUID,
        first_video: u32,
        all_streams: u32,
    ) -> Result<(IMFSourceReader, DecodeFmt, u32, u32, u64)> {
        use windows::Win32::Media::MediaFoundation::*;
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 3)
                .map_err(|e| anyhow!("MFCreateAttributes failed: {e}"))?;
            let attrs = attrs.unwrap();
            attrs.SetUINT32(mf_hw_guid, 1).ok();

            if let Ok(mgr) = create_dxgi_device_manager() {
                attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &mgr).ok();
                vprintln!("Video: D3D11 device manager attached (hardware decode enabled)");
            } else {
                vprintln!("Video: D3D11 device manager unavailable, falling back to software decode");
            }

            let reader: IMFSourceReader = MFCreateSourceReaderFromURL(url, Some(&attrs))
                .map_err(|e| anyhow!("Open '{source:?}' failed: {e}"))?;

            reader.SetStreamSelection(all_streams, false).ok();
            reader.SetStreamSelection(first_video, true).ok();

            // Validate native dimensions.
            let native_type: IMFMediaType = reader
                .GetNativeMediaType(first_video, 0)
                .map_err(|e| anyhow!("GetNativeMediaType failed: {e}"))?;
            let native_frame_size = native_type
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .map_err(|e| anyhow!("GetUINT64(MF_MT_FRAME_SIZE) failed: {e}"))?;
            if native_frame_size == 0
                || (native_frame_size >> 32) == 0
                || (native_frame_size & 0xFFFF_FFFF) == 0
            {
                return Err(anyhow!("Video has zero-size dimensions"));
            }

            let fmt = try_set_output_format(&reader, first_video)?;

            let out_type: IMFMediaType = reader
                .GetCurrentMediaType(first_video)
                .map_err(|e| anyhow!("GetCurrentMediaType failed: {e}"))?;
            let fs = out_type
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .map_err(|e| anyhow!("GetUINT64(MF_MT_FRAME_SIZE) output type failed: {e}"))?;
            let width  = (fs >> 32) as u32;
            let height = (fs & 0xFFFF_FFFF) as u32;

            vprintln!("Video: {width}x{height} (output), format: {fmt:?}");

            let duration_us = query_duration_us(&reader);

            Ok((reader, fmt, width, height, duration_us))
        }
    }

    // ── first open: send init message ────────────────────────────────────────
    let (reader, fmt, width, height, duration_us) =
        match open_reader(url, &source, &MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
                          FIRST_VIDEO, ALL_STREAMS) {
            Ok(r) => r,
            Err(e) => {
                let _ = init_tx.send(Err(e));
                return Ok(());
            }
        };
    let _ = init_tx.send(Ok((width, height, duration_us, fmt == DecodeFmt::Nv12)));

    // The reader is replaced on each loop iteration (reopen instead of seek at EOF).
    let mut reader = reader;
    let mut fmt    = fmt;

    // ── decode loop ────────────────────────────────────────────────────────
    let mut wall_start: Option<Instant> = None;
    let mut pts_start: i64 = 0;
    let mut decode_one = false; // allow one frame decode even while paused (for seek preview)
    let mut last_speed_index = speed_index.load(Ordering::Relaxed);
    // When Some(us), decode frames without displaying/pacing until pts_us >= us.
    let mut seek_until_us: Option<u64> = None;
    // Consecutive ReadSample calls that returned no frame (stream tick or null sample).
    // MF uses these instead of MF_SOURCE_READERF_ENDOFSTREAM for some sources
    // (SMB network drives, some HTTP servers).  A sustained run signals EOF.
    let mut null_sample_streak: u32 = 0;
    const NULL_STREAK_EOF: u32 = 30; // 30 × 1 ms ≈ 30 ms of silence → treat as EOF

    'decode: loop {
        // ── stop signal ───────────────────────────────────────────────────
        if stop.load(Ordering::Relaxed) {
            break 'decode;
        }

        // ── seek request ───────────────────────────────────────────────────
        {
            let target = seek_request.lock().unwrap().take();
            if let Some(us) = target {
                unsafe { seek_reader_to(&reader, us) };
                wall_start = None;
                seek_until_us = Some(us);
                decode_one = true; // show the seeked frame even if paused
                // Mute audio until the new wall clock is established at the
                // seek target frame, preventing stale/early audio from playing.
                audio_started.store(false, Ordering::Release);
            }
        }

        // ── speed change → reset pacing so we don't sleep for the accumulated deficit ──
        {
            let cur = speed_index.load(Ordering::Relaxed);
            if cur != last_speed_index {
                last_speed_index = cur;
                wall_start = None;
            }
        }

        // ── pause ──────────────────────────────────────────────────────────
        if paused.load(Ordering::Relaxed) && !decode_one {
            thread::sleep(Duration::from_millis(5));
            continue 'decode;
        }
        decode_one = false;

        // ── read next sample ───────────────────────────────────────────────
        let mut flags: u32 = 0;
        let mut timestamp: i64 = 0;
        let mut sample: Option<IMFSample> = None;

        let read_ok = unsafe {
            reader
                .ReadSample(
                    FIRST_VIDEO,
                    0,
                    None,
                    Some(&mut flags as *mut u32 as *mut _),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .is_ok()
        };

        if !read_ok {
            eprintln!("Video: ReadSample error (flags={flags:#010x})");
            break 'decode;
        }

        if flags & 0x01 != 0 {
            eprintln!("Video: MF_SOURCE_READERF_ERROR");
            break 'decode;
        }

        // EOF — reopen the source reader to loop cleanly.
        // Seeking after EOF is unreliable in MF (produces infinite stream ticks).
        // Check EOF *before* stream-tick: MF can return flags=0x06 (tick|EOF)
        // simultaneously; handling tick first with `continue` would skip this.
        if flags & 0x04 != 0 {
            vprintln!("Video: EOF detected, reopening reader for loop.");
            let seek_to = if loop_state.load(Ordering::Relaxed) == 2 {
                loop_start_us.load(Ordering::Relaxed)
            } else {
                0
            };
            match open_reader(url, &source, &MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
                             FIRST_VIDEO, ALL_STREAMS) {
                Ok((new_reader, new_fmt, _, _, _)) => {
                    reader = new_reader;
                    fmt    = new_fmt;
                    if seek_to > 0 {
                        unsafe { seek_reader_to(&reader, seek_to) };
                    }
                }
                Err(e) => {
                    eprintln!("Video: reopen failed on loop: {e}");
                    break 'decode;
                }
            }
            // Sync audio to the same loop point.
            audio_seek.store(seek_to, Ordering::Relaxed);
            audio_flush_gen.fetch_add(1, Ordering::Relaxed);
            wall_start   = None;
            seek_until_us = None;
            continue 'decode;
        }

        // No frame produced: stream tick (gap marker) or null sample.
        // MF uses these instead of MF_SOURCE_READERF_ENDOFSTREAM for some sources
        // (SMB network drives, some HTTP servers).  A sustained run means EOF.
        if flags & 0x02 != 0 || sample.is_none() {
            null_sample_streak += 1;
            if null_sample_streak >= NULL_STREAK_EOF {
                vprintln!("Video: stream-end detected via tick/null streak, reopening for loop.");
                let seek_to = if loop_state.load(Ordering::Relaxed) == 2 {
                    loop_start_us.load(Ordering::Relaxed)
                } else {
                    0
                };
                match open_reader(url, &source, &MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
                                 FIRST_VIDEO, ALL_STREAMS) {
                    Ok((new_reader, new_fmt, _, _, _)) => {
                        reader = new_reader;
                        fmt    = new_fmt;
                        if seek_to > 0 {
                            unsafe { seek_reader_to(&reader, seek_to) };
                        }
                    }
                    Err(e) => {
                        eprintln!("Video: reopen failed on loop: {e}");
                        break 'decode;
                    }
                }
                // Sync audio to the same loop point.
                audio_seek.store(seek_to, Ordering::Relaxed);
                audio_flush_gen.fetch_add(1, Ordering::Relaxed);
                null_sample_streak = 0;
                wall_start    = None;
                seek_until_us = None;
            } else {
                thread::sleep(Duration::from_millis(1));
            }
            continue 'decode;
        }
        null_sample_streak = 0;
        let sample = sample.unwrap();

        // ── loop-end check ─────────────────────────────────────────────────
        let pts_us = (timestamp / 10) as u64;
        if loop_state.load(Ordering::Relaxed) == 2 {
            let end = loop_end_us.load(Ordering::Relaxed);
            if pts_us >= end {
                let start = loop_start_us.load(Ordering::Relaxed);
                unsafe { seek_reader_to(&reader, start) };
                // Sync audio to the loop-start point.
                audio_seek.store(start, Ordering::Relaxed);
                audio_flush_gen.fetch_add(1, Ordering::Relaxed);
                wall_start = None;
                continue 'decode;
            }
        }

        current_pts_us.store(pts_us, Ordering::Relaxed);

        // ── seek catch-up: skip pacing and display until we reach target ──
        if let Some(until) = seek_until_us {
            if pts_us < until {
                // Still catching up — decode without displaying or sleeping.
                continue 'decode;
            }
            // Reached (or passed) the target: resume normal playback.
            seek_until_us = None;
            wall_start = None; // reset pacing so we don't try to make up lost time
            // The first displayed frame may be slightly past the requested seek
            // target due to keyframe alignment.  Sync audio to this exact PTS so
            // it starts from the same frame as video (not from the originally
            // requested position which may differ by up to one GOP).
            audio_seek.store(pts_us, Ordering::Relaxed);
            audio_flush_gen.fetch_add(1, Ordering::Relaxed);
        }

        // ── speed-adjusted pacing ──────────────────────────────────────────
        let speed = SPEEDS[speed_index.load(Ordering::Relaxed) as usize];
        match wall_start {
            None => {
                wall_start = Some(Instant::now());
                pts_start = timestamp;
                // Signal the audio callback that the video clock is now
                // established — it can start playing from this moment.
                audio_started.store(true, Ordering::Release);
            }
            Some(start) => {
                let pts_elapsed_ns = (timestamp - pts_start).max(0) as u64 * 100;
                // At `speed` = 0.5 we want to take 2× wall time per PTS unit.
                let target_wall_ns = (pts_elapsed_ns as f64 / speed as f64) as u64;
                let wall_ns = start.elapsed().as_nanos() as u64;
                if target_wall_ns > wall_ns + 1_000_000 {
                    thread::sleep(Duration::from_nanos(target_wall_ns - wall_ns - 500_000));
                }
            }
        }

        // ── frame extraction ──────────────────────────────────────────────
        // NV12: copy raw Y+UV planes; GPU shader handles YCbCr → RGB.
        // BGRA/YUY2: CPU converts to BGRA (only reached on HW-decode fallback).
        let frame = match fmt {
            DecodeFmt::Nv12 => {
                match extract_nv12_raw(&sample, height as usize) {
                    Ok(f) => f,
                    Err(e) => return Err(e),
                }
            }
            DecodeFmt::Bgra => {
                match extract_bgra_via_2d(&sample, width as usize, height as usize) {
                    Ok(data) => VideoFrame { data, format: VideoFormat::Bgra },
                    Err(e) => return Err(e),
                }
            }
            DecodeFmt::Yuy2 => {
                let raw = lock_contiguous(&sample)?;
                let data = yuy2_to_bgra(&raw, width as usize, height as usize);
                VideoFrame { data, format: VideoFormat::Bgra }
            }
        };

        *latest.lock().unwrap() = Some(frame);
    }

    Ok(())
}

fn try_set_output_format(
    reader: &windows::Win32::Media::MediaFoundation::IMFSourceReader,
    stream: u32,
) -> Result<DecodeFmt> {
    use windows::Win32::Media::MediaFoundation::*;

    // NV12 first: hardware decoders output it natively (no CPU conversion).
    // BGRA/YUY2 are fallbacks for software-only or exotic codecs.
    let candidates: &[(*const windows::core::GUID, DecodeFmt)] = &[
        (&MFVideoFormat_NV12,   DecodeFmt::Nv12),
        (&MFVideoFormat_ARGB32, DecodeFmt::Bgra),
        (&MFVideoFormat_RGB32,  DecodeFmt::Bgra),
        (&MFVideoFormat_YUY2,   DecodeFmt::Yuy2),
    ];

    for &(subtype_ptr, decode_fmt) in candidates {
        let ok = unsafe {
            let out_type: IMFMediaType =
                MFCreateMediaType().map_err(|e| anyhow!("MFCreateMediaType: {e}"))?;
            out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok();
            out_type.SetGUID(&MF_MT_SUBTYPE, &*subtype_ptr).ok();
            reader.SetCurrentMediaType(stream, None, &out_type).is_ok()
        };
        if ok {
            return Ok(decode_fmt);
        }
    }

    Err(anyhow!("No supported output format (tried ARGB32, RGB32, NV12, YUY2)"))
}

// ── frame extraction helpers ──────────────────────────────────────────────

fn extract_bgra_via_2d(
    sample: &windows::Win32::Media::MediaFoundation::IMFSample,
    width: usize,
    height: usize,
) -> Result<Vec<u8>> {
    use windows::Win32::Media::MediaFoundation::IMF2DBuffer;
    use windows::core::Interface as _;

    let row_bytes = width * 4;
    let mut out = vec![0u8; row_bytes * height];

    let ok = unsafe {
        let buf = sample
            .GetBufferByIndex(0)
            .map_err(|e| anyhow!("GetBufferByIndex: {e}"))?;

        if let Ok(buf2d) = buf.cast::<IMF2DBuffer>() {
            let mut scan0: *mut u8 = std::ptr::null_mut();
            let mut pitch: i32 = 0;
            buf2d.Lock2D(&mut scan0, &mut pitch)
                .map_err(|e| anyhow!("Lock2D: {e}"))?;

            for row in 0..height {
                let src = scan0.offset(row as isize * pitch as isize);
                let dst = out.as_mut_ptr().add(row * row_bytes);
                std::ptr::copy_nonoverlapping(src, dst, row_bytes);
            }

            buf2d.Unlock2D().ok();
            true
        } else {
            false
        }
    };

    if !ok {
        let raw = lock_contiguous(sample)?;
        let copy = out.len().min(raw.len());
        out[..copy].copy_from_slice(&raw[..copy]);
    }

    Ok(out)
}

/// Copy raw NV12 data from an IMF2DBuffer without any CPU colour conversion.
/// Returns a `VideoFrame` containing the Y plane (`height × stride` bytes) followed
/// immediately by the UV plane (`height/2 × stride` bytes).  The GPU shader
/// performs the YCbCr → RGB conversion.
fn extract_nv12_raw(
    sample: &windows::Win32::Media::MediaFoundation::IMFSample,
    height: usize,
) -> Result<VideoFrame> {
    use windows::Win32::Media::MediaFoundation::{IMF2DBuffer, IMF2DBuffer2, MF2DBuffer_LockFlags_Read};
    use windows::core::Interface as _;

    let buf = unsafe {
        sample
            .GetBufferByIndex(0)
            .map_err(|e| anyhow!("GetBufferByIndex (NV12): {e}"))?
    };

    let mut scan0: *mut u8 = std::ptr::null_mut();
    let mut pitch: i32 = 0;

    // Prefer IMF2DBuffer2::Lock2DSize — it returns the physical buffer length,
    // which lets us derive coded_height precisely even when GetCurrentLength()
    // reports only the display-region size.  Fall back to IMF2DBuffer::Lock2D
    // + GetCurrentLength() on older runtimes where IMF2DBuffer2 is absent.
    let (physical_len, buf2d) = unsafe {
        if let Ok(b2) = buf.cast::<IMF2DBuffer2>() {
            let mut buf_start: *mut u8 = std::ptr::null_mut();
            let mut buf_size:  u32 = 0;
            if b2.Lock2DSize(
                MF2DBuffer_LockFlags_Read,
                &mut scan0,
                &mut pitch,
                &mut buf_start,
                &mut buf_size,
            ).is_ok() {
                // pcbBufferLength covers the full physical NV12 allocation
                // (coded_height × stride × 3/2).  Adjust for any bytes that
                // precede scan0 so the arithmetic below stays correct.
                let pre = (scan0 as usize).saturating_sub(buf_start as usize);
                let effective = (buf_size as usize).saturating_sub(pre);
                (effective, buf.cast::<IMF2DBuffer>().ok())
            } else {
                (0usize, buf.cast::<IMF2DBuffer>().ok())
            }
        } else {
            let b = buf.cast::<IMF2DBuffer>()
                .map_err(|e| anyhow!("NV12 buffer is not IMF2DBuffer: {e}"))?;
            b.Lock2D(&mut scan0, &mut pitch)
                .map_err(|e| anyhow!("Lock2D (NV12): {e}"))?;
            let cur = buf.GetCurrentLength().unwrap_or(0) as usize;
            (cur, Some(b))
        }
    };

    let stride = pitch.unsigned_abs() as usize;

    // Determine where the UV plane really starts.
    // Hardware decoders pad coded_height to a block boundary (H.264: 16 px,
    // HEVC: 64 px), so coded_height ≥ display_height.  The UV plane starts at
    // coded_height × stride, not display_height × stride.
    //
    // Derive coded_height from the physical NV12 buffer length:
    //   total = coded_height × stride × 3/2
    //   => uv_offset = coded_height × stride = total × 2/3
    let uv_offset = if stride > 0 && physical_len >= height * stride {
        let total_rows = physical_len / stride; // coded_height × 3/2
        (total_rows / 3 * 2) * stride           // coded_height × stride
    } else {
        height * stride  // fallback: correct when coded_height == display_height
    };

    let uv_bytes = (height / 2) * stride;
    let copy_len = uv_offset + uv_bytes;
    let mut data = vec![0u8; copy_len];
    unsafe {
        std::ptr::copy_nonoverlapping(scan0, data.as_mut_ptr(), copy_len);
        if let Some(b) = buf2d { b.Unlock2D().ok(); }
    }

    Ok(VideoFrame { data, format: VideoFormat::Nv12 { stride, uv_offset } })
}

fn lock_contiguous(
    sample: &windows::Win32::Media::MediaFoundation::IMFSample,
) -> Result<Vec<u8>> {
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| anyhow!("ConvertToContiguousBuffer: {e}"))?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len: u32 = 0;
        buffer.Lock(&mut ptr, None, Some(&mut len))
            .map_err(|e| anyhow!("IMFMediaBuffer::Lock: {e}"))?;
        let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        buffer.Unlock().ok();
        Ok(bytes)
    }
}

// ── audio decode thread ───────────────────────────────────────────────────

fn audio_decode_thread(
    source: String,
    format_tx: std::sync::mpsc::Sender<Option<(u32, u16)>>,
    producer: std::sync::mpsc::SyncSender<f32>,
    paused: Arc<AtomicBool>,
    speed_index: Arc<AtomicU32>,
    audio_seek: Arc<AtomicU64>,
    audio_flush_gen: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::*;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        const MF_VER: u32 = 0x0002_0070;
        // 0 = MFSTARTUP_FULL — needed for http(s):// URLs.
        if MFStartup(MF_VER, 0).is_err() {
            let _ = format_tx.send(None);
            CoUninitialize();
            return;
        }
        match open_and_decode_audio(
            source, &format_tx, &producer,
            &paused, &speed_index, &audio_seek, &audio_flush_gen, &stop,
        ) {
            Ok(()) => {}
            Err(e) => eprintln!("Audio decoder error: {e}"),
        }
        let _ = MFShutdown();
        CoUninitialize();
    }
}

fn open_and_decode_audio(
    source: String,
    format_tx: &std::sync::mpsc::Sender<Option<(u32, u16)>>,
    producer: &std::sync::mpsc::SyncSender<f32>,
    _paused: &Arc<AtomicBool>,
    _speed_index: &Arc<AtomicU32>,
    audio_seek: &Arc<AtomicU64>,
    _audio_flush_gen: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
) -> Result<()> {
    use windows::Win32::Media::MediaFoundation::*;
    use windows::core::PCWSTR;

    const FIRST_AUDIO: u32 = 0xFFFF_FFFD; // MF_SOURCE_READER_FIRST_AUDIO_STREAM
    const ALL_STREAMS: u32 = 0xFFFF_FFFE;
    const NO_SEEK: u64 = u64::MAX;

    let wide: Vec<u16> = if source.starts_with("http://") || source.starts_with("https://") {
        source.encode_utf16().chain([0u16]).collect()
    } else {
        use std::os::windows::ffi::OsStrExt;
        std::path::Path::new(&source).as_os_str().encode_wide().chain([0u16]).collect()
    };
    let url = PCWSTR(wide.as_ptr());

    let reader: IMFSourceReader = unsafe {
        MFCreateSourceReaderFromURL(url, None)
            .map_err(|e| anyhow!("Audio: open failed: {e}"))?
    };

    unsafe {
        reader.SetStreamSelection(ALL_STREAMS, false).ok();
        if reader.SetStreamSelection(FIRST_AUDIO, true).is_err() {
            eprintln!("Audio: no audio stream found in {source}");
            let _ = format_tx.send(None);
            return Ok(());
        }
    }



    // Request 32-bit float PCM output.
    let format_ok = unsafe {
        let out_type = MFCreateMediaType()
            .map_err(|e| anyhow!("MFCreateMediaType: {e}"))?;
        out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok();
        out_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).ok();
        out_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32).ok();
        let r = reader.SetCurrentMediaType(FIRST_AUDIO, None, &out_type);
        if r.is_err() { eprintln!("Audio: SetCurrentMediaType(Float) failed: {r:?}"); }
        r.is_ok()
    };

    if !format_ok {
        let _ = format_tx.send(None);
        return Ok(());
    }

    let (sample_rate, channels) = unsafe {
        let cur = reader
            .GetCurrentMediaType(FIRST_AUDIO)
            .map_err(|e| anyhow!("GetCurrentMediaType (audio): {e}"))?;
        let sr = cur.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48_000);
        let ch = cur.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2) as u16;
        (sr, ch)
    };
    let _ = format_tx.send(Some((sample_rate, channels)));

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // Handle seek request from the main thread.
        let seek_us = audio_seek.load(Ordering::Relaxed);
        if seek_us != NO_SEEK
            && audio_seek
                .compare_exchange(seek_us, NO_SEEK, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            unsafe { seek_reader_to(&reader, seek_us) };
            // flush_gen was already bumped by app.rs; cpal callback will drain stale samples.
        }

        // Throttle: if the channel is full, wait for the consumer to catch up.
        // (sync_channel capacity is the full ~2 s ring; full means we're well ahead.)
        // We detect fullness by a failed try_send below; no pre-check needed here.

        // Read next audio sample from MF.
        let mut flags: u32 = 0;
        let mut timestamp: i64 = 0;
        let mut sample: Option<IMFSample> = None;
        let read_ok = unsafe {
            reader
                .ReadSample(
                    FIRST_AUDIO,
                    0,
                    None,
                    Some(&mut flags as *mut u32 as *mut _),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .is_ok()
        };
        if !read_ok || flags & 0x01 != 0 {
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        if flags & 0x04 != 0 {
            // EOF — seek back to the start and keep filling.  The video decode
            // thread owns loop synchronisation: it will set audio_seek and bump
            // audio_flush_gen when it loops, so the cpal callback flushes stale
            // audio at the correct moment.  Don't flush here — that would drain
            // samples the cpal thread hasn't played yet.
            unsafe { seek_reader_to(&reader, 0) };
            continue;
        }

        let Some(sample) = sample else { continue };

        let bytes = lock_contiguous(&sample)?;
        // Interpret raw bytes as little-endian f32 PCM.
        let floats: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        // Push all floats into the channel, yielding when it is full.
        for &sample in floats.iter() {
            // Abort if a seek or stop arrives — don't fill the channel with stale audio.
            if audio_seek.load(Ordering::Relaxed) != NO_SEEK
                || stop.load(Ordering::Relaxed)
            {
                break;
            }
            // Block-send: parks this thread until there is room.
            // The channel is disconnected only when VideoDecoder is dropped.
            if producer.send(sample).is_err() {
                return Ok(()); // consumer gone, shut down
            }
        }
    }

    Ok(())
}

fn yuy2_to_bgra(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut dst = vec![0u8; w * h * 4];

    for row in 0..h {
        for col in 0..w {
            let base = row * w * 2 + (col & !1) * 2;
            let y    = src[base + if col & 1 == 0 { 0 } else { 2 }] as i32 - 16;
            let u    = src[base + 1] as i32 - 128;
            let v    = src[base + 3] as i32 - 128;

            let c = 298 * y;
            let r = ((c           + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            let g = ((c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            let b = ((c + 516 * u           + 128) >> 8).clamp(0, 255) as u8;

            let i = (row * w + col) * 4;
            dst[i]     = b;
            dst[i + 1] = g;
            dst[i + 2] = r;
            dst[i + 3] = 255;
        }
    }
    dst
}
