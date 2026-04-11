use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use anyhow::{anyhow, Result};

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
    pub width: u32,
    pub height: u32,
    _thread: thread::JoinHandle<()>,
}

impl VideoDecoder {
    pub fn open(path: PathBuf) -> Result<Self> {
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(u32, u32)>>();
        let latest: Arc<Mutex<Option<VideoFrame>>> = Arc::new(Mutex::new(None));
        let latest_clone = Arc::clone(&latest);

        let handle = thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || decode_thread(path, init_tx, latest_clone))?;

        let (width, height) = init_rx
            .recv()
            .map_err(|_| anyhow!("Decoder thread exited before sending init result"))??;

        Ok(Self { latest, width, height, _thread: handle })
    }

    /// Take the latest decoded frame, leaving the slot empty.
    /// Returns `None` if no new frame has arrived since the last call.
    pub fn take_frame(&self) -> Option<VideoFrame> {
        self.latest.lock().unwrap().take()
    }
}

// ── background decode thread ───────────────────────────────────────────────

fn decode_thread(
    path: PathBuf,
    init_tx: std::sync::mpsc::Sender<Result<(u32, u32)>>,
    latest: Arc<Mutex<Option<VideoFrame>>>,
) {
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::*;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        // MF_VERSION = (MF_SDK_VERSION << 16 | MF_API_VERSION) = 0x00020070
        const MF_VER: u32 = 0x0002_0070;
        const MFSTARTUP_NOSOCKET: u32 = 0x1;

        if let Err(e) = MFStartup(MF_VER, MFSTARTUP_NOSOCKET) {
            let _ = init_tx.send(Err(anyhow!("MFStartup failed: {e}")));
            CoUninitialize();
            return;
        }

        match open_and_decode(path, init_tx, &latest) {
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
    Bgra,  // ARGB32 or RGB32 — upload directly
    Nv12,  // convert to BGRA on the decode thread
    Yuy2,  // convert to BGRA on the decode thread
}

fn open_and_decode(
    path: PathBuf,
    init_tx: std::sync::mpsc::Sender<Result<(u32, u32)>>,
    latest: &Arc<Mutex<Option<VideoFrame>>>,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::core::PCWSTR;

    // MF_SOURCE_READER_FIRST_VIDEO_STREAM
    const FIRST_VIDEO: u32 = 0xFFFF_FFFC;
    // MF_SOURCE_READER_ALL_STREAMS
    const ALL_STREAMS: u32 = 0xFFFF_FFFE;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0u16]).collect();
    let url = PCWSTR(wide.as_ptr());

    // Create source reader with both video-processing flags so ARGB32 output
    // can be requested even when the decoder natively produces YUV.
    let reader: IMFSourceReader = unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2)
            .map_err(|e| anyhow!("MFCreateAttributes failed: {e}"))?;
        let attrs = attrs.unwrap();
        // MSDN: do NOT set both flags; ADVANCED_VIDEO_PROCESSING is the newer
        // superset (supports GPU acceleration and format conversion).
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1).ok();
        MFCreateSourceReaderFromURL(url, Some(&attrs))
            .map_err(|e| anyhow!("Open '{path:?}' failed: {e}"))?
    };

    // Select only the first video stream.
    unsafe {
        reader.SetStreamSelection(ALL_STREAMS, false).ok();
        reader.SetStreamSelection(FIRST_VIDEO, true).ok();
    }

    // Query dimensions from the native (compressed) media type.
    let frame_size: u64 = unsafe {
        let native_type: IMFMediaType = reader
            .GetNativeMediaType(FIRST_VIDEO, 0)
            .map_err(|e| anyhow!("GetNativeMediaType failed: {e}"))?;
        native_type
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map_err(|e| anyhow!("GetUINT64(MF_MT_FRAME_SIZE) failed: {e}"))?
    };
    let width = (frame_size >> 32) as u32;
    let height = (frame_size & 0xFFFF_FFFF) as u32;

    if width == 0 || height == 0 {
        let _ = init_tx.send(Err(anyhow!("Video has zero-size dimensions")));
        return Ok(());
    }

    // Try output formats in preference order.
    // ARGB32 / RGB32 can be uploaded to a Bgra8Unorm texture without conversion.
    // NV12 / YUY2 need CPU conversion but are always supported by the decoder.
    let fmt = try_set_output_format(&reader, FIRST_VIDEO)?;
    println!("Video: {width}x{height}, output format: {fmt:?}");

    // Init done — signal main thread.
    let _ = init_tx.send(Ok((width, height)));

    // ── decode loop ────────────────────────────────────────────────────────
    let mut wall_start: Option<Instant> = None;
    let mut pts_start: i64 = 0;

    loop {
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

        // MF_SOURCE_READERF_ENDOFSTREAM = 0x04
        if flags & 0x04 != 0 {
            break;
        }

        let Some(sample) = sample else {
            thread::sleep(Duration::from_millis(1));
            continue;
        };

        // PTS-based pacing so the video plays at the right speed.
        match wall_start {
            None => {
                wall_start = Some(Instant::now());
                pts_start = timestamp;
            }
            Some(start) => {
                let pts_ns = (timestamp - pts_start).max(0) as u64 * 100;
                let wall_ns = start.elapsed().as_nanos() as u64;
                if pts_ns > wall_ns + 1_000_000 {
                    thread::sleep(Duration::from_nanos(pts_ns - wall_ns - 500_000));
                }
            }
        }

        // Extract and convert to tightly-packed top-down BGRA.
        let bgra = match fmt {
            DecodeFmt::Bgra => {
                extract_bgra_via_2d(&sample, width as usize, height as usize)?
            }
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

    // Formats to try, paired with the DecodeFmt we'll use if successful.
    // ARGB32 and RGB32 both store BGRA in memory, so no conversion needed.
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

/// Copy the BGRA frame via `IMF2DBuffer::Lock2D`, which gives us the real row
/// pitch.  `pbScanline0` always points to the *visually top* row; a negative
/// pitch means the image is stored bottom-up in memory.  We copy row-by-row so
/// the result is always tightly-packed and top-down regardless of the source
/// layout.
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
        // Try to get the first buffer as IMF2DBuffer (most GPU-produced buffers).
        let buf = sample
            .GetBufferByIndex(0)
            .map_err(|e| anyhow!("GetBufferByIndex: {e}"))?;

        if let Ok(buf2d) = buf.cast::<IMF2DBuffer>() {
            let mut scan0: *mut u8 = std::ptr::null_mut();
            let mut pitch: i32 = 0;
            buf2d
                .Lock2D(&mut scan0, &mut pitch)
                .map_err(|e| anyhow!("Lock2D: {e}"))?;

            for row in 0..height {
                // scan0 + row*pitch is the start of the visually N-th row.
                // Works for both positive (top-down) and negative (bottom-up) pitch.
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
        // Fallback: contiguous buffer (no stride info, hope it's already tight).
        let raw = lock_contiguous(sample)?;
        let copy = out.len().min(raw.len());
        out[..copy].copy_from_slice(&raw[..copy]);
    }

    Ok(out)
}

/// Lock a sample as a flat contiguous buffer and return a copy of the bytes.
fn lock_contiguous(
    sample: &windows::Win32::Media::MediaFoundation::IMFSample,
) -> Result<Vec<u8>> {
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| anyhow!("ConvertToContiguousBuffer: {e}"))?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len: u32 = 0;
        buffer
            .Lock(&mut ptr, None, Some(&mut len))
            .map_err(|e| anyhow!("IMFMediaBuffer::Lock: {e}"))?;
        let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        buffer.Unlock().ok();
        Ok(bytes)
    }
}

// ── software colour-space conversions ─────────────────────────────────────

/// NV12 → BGRA  (BT.601 limited range)
/// NV12 layout: Y plane (w×h bytes) followed by interleaved UV plane (w×h/2 bytes).
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

/// YUY2 → BGRA  (BT.601 limited range)
/// YUY2 layout: Y0 U0 Y1 V0 per pair of pixels.
fn yuy2_to_bgra(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut dst = vec![0u8; w * h * 4];

    for row in 0..h {
        for col in 0..w {
            let base  = row * w * 2 + (col & !1) * 2;
            let y     = src[base + if col & 1 == 0 { 0 } else { 2 }] as i32 - 16;
            let u     = src[base + 1] as i32 - 128;
            let v     = src[base + 3] as i32 - 128;

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
