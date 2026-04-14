use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    mpsc::Receiver,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::ui::control_bar::SPEEDS;

// ── OLA time-stretcher ────────────────────────────────────────────────────────

/// Overlap-Add time-scale modifier.
///
/// Slows audio without pitch-shifting by using a shorter analysis hop (H_a)
/// than the synthesis hop (H_s):
///
///   H_a = H_s × speed   (< H_s for slowdown)
///
/// A Hann window applied at 50 % overlap gives clean crossfades.
/// Only handles slowdown (speed ≤ 1); 1× speed bypasses this entirely.
struct Ola {
    ch:       usize,       // interleaved channel count
    frame_sz: usize,       // analysis window in per-channel samples  (2048)
    hop_out:  usize,       // synthesis hop  = frame_sz / 2           (1024)
    window:   Vec<f32>,    // Hann window [frame_sz]

    in_buf:  VecDeque<f32>, // raw interleaved input (fed from rx)
    in_pos:  f64,           // fractional per-channel read position within in_buf

    overlap:    Vec<f32>, // windowed second half of previous frame [hop_out × ch]
    out_buf:    Vec<f32>, // current output chunk ready to consume  [hop_out × ch]
    out_cursor: usize,    // samples already consumed from out_buf
}

impl Ola {
    fn new(ch: usize) -> Self {
        const FRAME: usize = 2048;
        let hop = FRAME / 2;
        let window: Vec<f32> = (0..FRAME)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / (FRAME - 1) as f32).cos()))
            .collect();
        Self {
            ch,
            frame_sz: FRAME,
            hop_out:  hop,
            window,
            in_buf:    VecDeque::new(),
            in_pos:    0.0,
            overlap:   vec![0.0; hop * ch],
            out_buf:   vec![0.0; hop * ch],
            out_cursor: hop, // start exhausted so first fill() triggers a frame
        }
    }

    /// Discard all state — called on seek or when returning to 1× speed.
    fn reset(&mut self) {
        self.in_buf.clear();
        self.in_pos = 0.0;
        self.overlap.fill(0.0);
        self.out_buf.fill(0.0);
        self.out_cursor = self.hop_out;
    }

    /// Compute one output chunk (hop_out × ch samples).
    fn process_frame(&mut self, speed: f32) {
        let ch       = self.ch;
        let frame_sz = self.frame_sz;
        let hop_out  = self.hop_out;
        let base     = self.in_pos as usize;

        if self.in_buf.len() < (base + frame_sz) * ch {
            // Underrun: output silence, don't advance.
            self.out_buf.fill(0.0);
            self.out_cursor = 0;
            return;
        }

        // Window the input frame and overlap-add in one pass.
        // First half  → out_buf (OLA with previous overlap tail).
        // Second half → new overlap tail for next frame.
        for i in 0..frame_sz {
            let w = self.window[i];
            for c in 0..ch {
                let s = self.in_buf[(base + i) * ch + c] * w;
                if i < hop_out {
                    self.out_buf[i * ch + c] = s + self.overlap[i * ch + c];
                } else {
                    self.overlap[(i - hop_out) * ch + c] = s;
                }
            }
        }
        self.out_cursor = 0;

        // Advance fractional input position by H_a = hop_out × speed.
        self.in_pos += hop_out as f64 * speed as f64;

        // Drop fully-consumed samples from the front of in_buf.
        let drop_frames  = self.in_pos as usize;
        let drop_samples = drop_frames * ch;
        if drop_samples <= self.in_buf.len() {
            self.in_buf.drain(..drop_samples);
            self.in_pos -= drop_frames as f64;
        }
    }

    /// Fill `output` (interleaved) with time-stretched audio from `rx`.
    fn fill(&mut self, output: &mut [f32], rx: &Receiver<f32>, speed: f32) {
        // Pull from rx up to a bounded lookahead (~4 frames) so in_buf
        // doesn't grow unboundedly as the decoder feeds at 1× rate.
        let target = 4 * self.frame_sz * self.ch;
        while self.in_buf.len() < target {
            match rx.try_recv() {
                Ok(s)  => self.in_buf.push_back(s),
                Err(_) => break,
            }
        }

        let mut pos = 0;
        while pos < output.len() {
            if self.out_cursor >= self.hop_out * self.ch {
                self.process_frame(speed);
            }
            let avail = (self.hop_out * self.ch) - self.out_cursor;
            let n     = avail.min(output.len() - pos);
            output[pos..pos + n]
                .copy_from_slice(&self.out_buf[self.out_cursor..self.out_cursor + n]);
            pos            += n;
            self.out_cursor += n;
        }
    }
}

// ── Linear resampler ─────────────────────────────────────────────────────────

/// Simple linear-interpolation resampler for 1× playback when the file sample
/// rate differs from the device rate.  Sounds transparent for small ratios.
struct LinearResampler {
    ratio: f64,          // file_sr / device_sr  (step in input per output sample)
    pos:   f64,          // fractional read position within buf (in frames)
    buf:   VecDeque<f32>, // interleaved input frames
    ch:    usize,
}

impl LinearResampler {
    fn new(file_sr: u32, device_sr: u32, ch: usize) -> Self {
        Self { ratio: file_sr as f64 / device_sr as f64, pos: 0.0, buf: VecDeque::new(), ch }
    }

    fn reset(&mut self) { self.buf.clear(); self.pos = 0.0; }

    fn fill(&mut self, output: &mut [f32], rx: &Receiver<f32>) {
        let ch = self.ch;
        let frames_out = output.len() / ch;
        for i in 0..frames_out {
            let i0   = self.pos as usize;
            let i1   = i0 + 1;
            let frac = (self.pos - i0 as f64) as f32;
            while self.buf.len() < (i1 + 1) * ch {
                for _ in 0..ch { self.buf.push_back(rx.try_recv().unwrap_or(0.0)); }
            }
            for c in 0..ch {
                let s0 = self.buf[i0 * ch + c];
                let s1 = self.buf[i1 * ch + c];
                output[i * ch + c] = s0 + (s1 - s0) * frac;
            }
            self.pos += self.ratio;
        }
        let drain = self.pos as usize;
        if drain > 0 && drain * ch <= self.buf.len() {
            self.buf.drain(..drain * ch);
            self.pos -= drain as f64;
        }
    }
}

// ── AudioPlayer ───────────────────────────────────────────────────────────────

pub struct AudioPlayer {
    _stream: cpal::Stream,
}

impl AudioPlayer {
    /// Start audio playback.  `rx` receives float32 interleaved PCM samples.
    /// `flush_gen` is incremented on every seek so stale buffered audio is
    /// discarded immediately.
    pub fn start(
        rx: Receiver<f32>,
        sample_rate: u32,
        channels: u16,
        paused: Arc<AtomicBool>,
        speed_index: Arc<AtomicU32>,
        flush_gen: Arc<AtomicU64>,
    ) -> Option<Self> {
        let host   = cpal::default_host();
        let device = host.default_output_device()?;

        // Use the device's preferred sample rate.  If the file rate differs,
        // fold the ratio into the OLA speed so pitch and timing stay correct.
        let device_sr = device.default_output_config()
            .map(|c| c.sample_rate().0)
            .unwrap_or(sample_rate);
        let file_sr = sample_rate;

        let config = cpal::StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(device_sr),
            buffer_size: cpal::BufferSize::Default,
        };

        eprintln!("Audio: device={:?} file_sr={file_sr} device_sr={device_sr} ch={channels}",
                  device.name());

        let mut last_flush = flush_gen.load(Ordering::Relaxed);
        let mut last_idx   = 0_usize;
        let mut ola        = Ola::new(channels as usize);
        let mut linear     = LinearResampler::new(file_sr, device_sr, channels as usize);

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // ── seek flush ───────────────────────────────────────────
                    let cur = flush_gen.load(Ordering::Relaxed);
                    if cur != last_flush {
                        last_flush = cur;
                        while rx.try_recv().is_ok() {}
                        ola.reset();
                        linear.reset();
                        for s in data.iter_mut() { *s = 0.0; }
                        return;
                    }

                    // ── pause ────────────────────────────────────────────────
                    if paused.load(Ordering::Relaxed) {
                        while rx.try_recv().is_ok() {}
                        for s in data.iter_mut() { *s = 0.0; }
                        return;
                    }

                    let idx = speed_index.load(Ordering::Relaxed) as usize;

                    // Reset OLA when transitioning back to 1×.
                    if idx == 0 && last_idx != 0 { ola.reset(); }
                    // Reset linear resampler when transitioning to slow-motion.
                    if idx != 0 && last_idx == 0 { linear.reset(); }
                    last_idx = idx;

                    if idx == 0 {
                        if device_sr == file_sr {
                            // ── 1× pass-through, rates match ─────────────────
                            for s in data.iter_mut() { *s = rx.try_recv().unwrap_or(0.0); }
                        } else {
                            // ── 1× with sample-rate conversion ───────────────
                            linear.fill(data, &rx);
                        }
                    } else {
                        // ── slow-motion OLA (fold resample ratio into speed) ──
                        let speed = SPEEDS[idx.min(SPEEDS.len() - 1)]
                            * (file_sr as f32 / device_sr as f32);
                        ola.fill(data, &rx, speed);
                    }
                },
                |err| eprintln!("Audio stream error: {err}"),
                None,
            );

        let stream = match stream {
            Ok(s)  => s,
            Err(e) => { eprintln!("Audio: build_output_stream failed: {e}"); return None; }
        };

        if let Err(e) = stream.play() {
            eprintln!("Audio: play() failed: {e}");
            return None;
        }
        eprintln!("Audio: stream started ok");
        Some(Self { _stream: stream })
    }
}
