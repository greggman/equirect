use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use anyhow::{anyhow, Result};

// Speed table (must match ui::control_bar::SPEEDS).
const SPEEDS: [f32; 5] = [1.0, 2.0 / 3.0, 0.5, 1.0 / 3.0, 0.25];

/// A decoded video frame — always BGRA, tightly packed, `width × height × 4` bytes.
#[derive(Clone)]
pub struct VideoFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Opens a video file and decodes it on a background thread via Windows Media Foundation.
/// The latest decoded frame (always BGRA) is available via `take_frame()`.
pub struct VideoDecoder {
    latest: Arc<Mutex<Option<VideoFrame>>>,
    /// Current presentation timestamp in microseconds, updated by the decode thread.
    pub current_pts_us: Arc<AtomicU64>,
    /// Duration of the video in microseconds (0 if not known).
    pub duration_us: u64,
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
    pub width: u32,
    pub height: u32,
    _thread: thread::JoinHandle<()>,
}

impl VideoDecoder {
    pub fn open(path: PathBuf) -> Result<Self> {
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(u32, u32, u64)>>();
        let latest: Arc<Mutex<Option<VideoFrame>>> = Arc::new(Mutex::new(None));
        let latest_clone = Arc::clone(&latest);
        let current_pts_us  = Arc::new(AtomicU64::new(0));
        let paused          = Arc::new(AtomicBool::new(false));
        let speed_index     = Arc::new(AtomicU32::new(0));
        let loop_state      = Arc::new(AtomicU8::new(0));
        let loop_start_us   = Arc::new(AtomicU64::new(0));
        let loop_end_us     = Arc::new(AtomicU64::new(0));
        let seek_request    = Arc::new(Mutex::new(None::<u64>));

        let pts_c   = Arc::clone(&current_pts_us);
        let pau_c   = Arc::clone(&paused);
        let spd_c   = Arc::clone(&speed_index);
        let lps_c   = Arc::clone(&loop_state);
        let lps_start_c = Arc::clone(&loop_start_us);
        let lps_end_c   = Arc::clone(&loop_end_us);
        let seek_c  = Arc::clone(&seek_request);

        let handle = thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                decode_thread(
                    path, init_tx, latest_clone,
                    pts_c, pau_c, spd_c,
                    lps_c, lps_start_c, lps_end_c, seek_c,
                )
            })?;

        let (width, height, duration_us) = init_rx
            .recv()
            .map_err(|_| anyhow!("Decoder thread exited before sending init result"))??;

        Ok(Self {
            latest,
            current_pts_us,
            duration_us,
            paused,
            speed_index,
            loop_state,
            loop_start_us,
            loop_end_us,
            seek_request,
            width,
            height,
            _thread: handle,
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
    path: PathBuf,
    init_tx: std::sync::mpsc::Sender<Result<(u32, u32, u64)>>,
    latest: Arc<Mutex<Option<VideoFrame>>>,
    current_pts_us:  Arc<AtomicU64>,
    paused:          Arc<AtomicBool>,
    speed_index:     Arc<AtomicU32>,
    loop_state:      Arc<AtomicU8>,
    loop_start_us:   Arc<AtomicU64>,
    loop_end_us:     Arc<AtomicU64>,
    seek_request:    Arc<Mutex<Option<u64>>>,
) {
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::*;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        const MF_VER: u32 = 0x0002_0070;
        const MFSTARTUP_NOSOCKET: u32 = 0x1;

        if let Err(e) = MFStartup(MF_VER, MFSTARTUP_NOSOCKET) {
            let _ = init_tx.send(Err(anyhow!("MFStartup failed: {e}")));
            CoUninitialize();
            return;
        }

        match open_and_decode(
            path, init_tx, &latest,
            &current_pts_us, &paused, &speed_index,
            &loop_state, &loop_start_us, &loop_end_us, &seek_request,
        ) {
            Ok(()) => {}
            Err(e) => eprintln!("Video decoder error: {e}"),
        }

        let _ = MFShutdown();
        CoUninitialize();
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
    path: PathBuf,
    init_tx: std::sync::mpsc::Sender<Result<(u32, u32, u64)>>,
    latest: &Arc<Mutex<Option<VideoFrame>>>,
    current_pts_us:  &Arc<AtomicU64>,
    paused:          &Arc<AtomicBool>,
    speed_index:     &Arc<AtomicU32>,
    loop_state:      &Arc<AtomicU8>,
    loop_start_us:   &Arc<AtomicU64>,
    loop_end_us:     &Arc<AtomicU64>,
    seek_request:    &Arc<Mutex<Option<u64>>>,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::core::PCWSTR;

    const FIRST_VIDEO: u32 = 0xFFFF_FFFC;
    const ALL_STREAMS: u32 = 0xFFFF_FFFE;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0u16]).collect();
    let url = PCWSTR(wide.as_ptr());

    let reader: IMFSourceReader = unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2)
            .map_err(|e| anyhow!("MFCreateAttributes failed: {e}"))?;
        let attrs = attrs.unwrap();
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1).ok();
        MFCreateSourceReaderFromURL(url, Some(&attrs))
            .map_err(|e| anyhow!("Open '{path:?}' failed: {e}"))?
    };

    unsafe {
        reader.SetStreamSelection(ALL_STREAMS, false).ok();
        reader.SetStreamSelection(FIRST_VIDEO, true).ok();
    }

    let frame_size: u64 = unsafe {
        let native_type: IMFMediaType = reader
            .GetNativeMediaType(FIRST_VIDEO, 0)
            .map_err(|e| anyhow!("GetNativeMediaType failed: {e}"))?;
        native_type
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map_err(|e| anyhow!("GetUINT64(MF_MT_FRAME_SIZE) failed: {e}"))?
    };
    let width  = (frame_size >> 32) as u32;
    let height = (frame_size & 0xFFFF_FFFF) as u32;

    if width == 0 || height == 0 {
        let _ = init_tx.send(Err(anyhow!("Video has zero-size dimensions")));
        return Ok(());
    }

    let fmt = try_set_output_format(&reader, FIRST_VIDEO)?;
    println!("Video: {width}x{height}, output format: {fmt:?}");

    let duration_us = unsafe { query_duration_us(&reader) };

    let _ = init_tx.send(Ok((width, height, duration_us)));

    // ── decode loop ────────────────────────────────────────────────────────
    let mut wall_start: Option<Instant> = None;
    let mut pts_start: i64 = 0;
    let mut decode_one = false; // allow one frame decode even while paused (for seek preview)
    let mut last_speed_index = speed_index.load(Ordering::Relaxed);

    'decode: loop {
        // ── seek request ───────────────────────────────────────────────────
        {
            let target = seek_request.lock().unwrap().take();
            if let Some(us) = target {
                unsafe { seek_reader_to(&reader, us) };
                wall_start = None;
                decode_one = true; // show the seeked frame even if paused
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

        unsafe {
            reader
                .ReadSample(
                    FIRST_VIDEO,
                    0,
                    None,
                    Some(&mut flags as *mut u32 as *mut _),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .map_err(|e| anyhow!("ReadSample failed: {e}"))?;
        }

        // EOF
        if flags & 0x04 != 0 {
            let ls = loop_state.load(Ordering::Relaxed);
            if ls == 2 {
                let start = loop_start_us.load(Ordering::Relaxed);
                unsafe { seek_reader_to(&reader, start) };
                wall_start = None;
                continue 'decode;
            }
            break 'decode;
        }

        let Some(sample) = sample else {
            thread::sleep(Duration::from_millis(1));
            continue 'decode;
        };

        // ── loop-end check ─────────────────────────────────────────────────
        let pts_us = (timestamp / 10) as u64;
        if loop_state.load(Ordering::Relaxed) == 2 {
            let end = loop_end_us.load(Ordering::Relaxed);
            if pts_us >= end {
                let start = loop_start_us.load(Ordering::Relaxed);
                unsafe { seek_reader_to(&reader, start) };
                wall_start = None;
                continue 'decode;
            }
        }

        current_pts_us.store(pts_us, Ordering::Relaxed);

        // ── speed-adjusted pacing ──────────────────────────────────────────
        let speed = SPEEDS[speed_index.load(Ordering::Relaxed) as usize];
        match wall_start {
            None => {
                wall_start = Some(Instant::now());
                pts_start = timestamp;
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

        // ── pixel format conversion ────────────────────────────────────────
        let bgra = match fmt {
            DecodeFmt::Bgra => extract_bgra_via_2d(&sample, width as usize, height as usize)?,
            DecodeFmt::Nv12 | DecodeFmt::Yuy2 => {
                let raw = lock_contiguous(&sample)?;
                if fmt == DecodeFmt::Nv12 {
                    nv12_to_bgra(&raw, width as usize, height as usize)
                } else {
                    yuy2_to_bgra(&raw, width as usize, height as usize)
                }
            }
        };

        *latest.lock().unwrap() = Some(VideoFrame { data: bgra, width, height });
    }

    Ok(())
}

fn try_set_output_format(
    reader: &windows::Win32::Media::MediaFoundation::IMFSourceReader,
    stream: u32,
) -> Result<DecodeFmt> {
    use windows::Win32::Media::MediaFoundation::*;

    let candidates: &[(*const windows::core::GUID, DecodeFmt)] = &[
        (&MFVideoFormat_ARGB32, DecodeFmt::Bgra),
        (&MFVideoFormat_RGB32,  DecodeFmt::Bgra),
        (&MFVideoFormat_NV12,   DecodeFmt::Nv12),
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

// ── software colour-space conversions ─────────────────────────────────────

fn nv12_to_bgra(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut dst = vec![0u8; w * h * 4];
    let y_plane  = &src[..w * h];
    let uv_plane = &src[w * h..];

    for row in 0..h {
        for col in 0..w {
            let y  = y_plane[row * w + col] as i32 - 16;
            let ui = (row / 2) * w + (col & !1);
            let u  = uv_plane[ui]     as i32 - 128;
            let v  = uv_plane[ui + 1] as i32 - 128;

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
