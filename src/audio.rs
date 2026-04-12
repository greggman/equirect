use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    mpsc::Receiver,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct AudioPlayer {
    _stream: cpal::Stream,
}

impl AudioPlayer {
    /// Start audio playback.  `rx` receives float32 interleaved PCM samples from the
    /// audio decode thread via a bounded sync_channel.  `flush_gen` is incremented on
    /// every seek so the callback drains stale buffered samples immediately.
    pub fn start(
        rx: Receiver<f32>,
        sample_rate: u32,
        channels: u16,
        paused: Arc<AtomicBool>,
        speed_index: Arc<AtomicU32>,
        flush_gen: Arc<AtomicU64>,
    ) -> Option<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        let config = cpal::StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let mut last_flush = flush_gen.load(Ordering::Relaxed);

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // Flush stale audio on seek.
                    let cur = flush_gen.load(Ordering::Relaxed);
                    if cur != last_flush {
                        last_flush = cur;
                        while rx.try_recv().is_ok() {}
                        for s in data.iter_mut() {
                            *s = 0.0;
                        }
                        return;
                    }

                    // Drain and silence when paused or at non-1× speed.
                    if paused.load(Ordering::Relaxed) || speed_index.load(Ordering::Relaxed) != 0 {
                        while rx.try_recv().is_ok() {}
                        for s in data.iter_mut() {
                            *s = 0.0;
                        }
                        return;
                    }

                    for s in data.iter_mut() {
                        *s = rx.try_recv().unwrap_or(0.0);
                    }
                },
                |err| eprintln!("Audio stream error: {err}"),
                None,
            )
            .ok()?;

        stream.play().ok()?;
        Some(Self { _stream: stream })
    }
}
