//! Sound out of the machine: 48 kHz stereo samples, to a WAV file (headless
//! runs) or to the sound card (`play`).
//!
//! The sound card keeps its own clock, and the game is paced to 59.94
//! fields a second by the host's: between the two sits a buffer that the
//! sound card reads through a resampler whose rate is nudged, by at most
//! half a percent, to keep the buffer near its target. When the machine
//! falls behind real time the buffer runs dry and the sound card gets
//! silence; the sound is never stretched. Like PCSX2, the sound card's
//! samples pass a DC-blocking filter (a voice left playing at a fixed
//! level otherwise offsets the output); WAV files get the chip's own.

use std::collections::VecDeque;
use std::io::{Seek as _, SeekFrom, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};

/// The SPU2's output rate.
pub(crate) const RATE: u32 = 48_000;

/// A 16-bit stereo WAV file, written as samples come.
pub(crate) struct Wav {
    out: std::io::BufWriter<std::fs::File>,
    frames: u32,
}

impl Wav {
    /// Starts a WAV file at `path`.
    pub(crate) fn create(path: &Path) -> Result<Self> {
        let file =
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut wav = Self {
            out: std::io::BufWriter::new(file),
            frames: 0,
        };
        wav.header()?;
        Ok(wav)
    }

    fn header(&mut self) -> Result<()> {
        let data = self.frames * 4;
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(36 + data).to_le_bytes());
        h.extend_from_slice(b"WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes()); // PCM
        h.extend_from_slice(&2u16.to_le_bytes()); // stereo
        h.extend_from_slice(&RATE.to_le_bytes());
        h.extend_from_slice(&(RATE * 4).to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&data.to_le_bytes());
        self.out.write_all(&h)?;
        Ok(())
    }

    /// Appends samples.
    pub(crate) fn write(&mut self, samples: &[[i16; 2]]) -> Result<()> {
        for [l, r] in samples {
            self.out.write_all(&l.to_le_bytes())?;
            self.out.write_all(&r.to_le_bytes())?;
        }
        self.frames += u32::try_from(samples.len()).unwrap_or(0);
        Ok(())
    }

    /// Writes the sizes into the header and closes the file.
    pub(crate) fn finish(mut self) -> Result<()> {
        self.out.flush()?;
        self.out.seek(SeekFrom::Start(0))?;
        self.header()?;
        self.out.flush()?;
        Ok(())
    }
}

/// Samples waiting for the sound card, and how far into them it has read.
struct Queue {
    samples: VecDeque<[f32; 2]>,
    /// The position between the first two samples, 0 to 1.
    phase: f64,
    /// Samples the queue aims to hold.
    target: usize,
    /// The DC filter's last input and output, per channel.
    dc: [[f32; 2]; 2],
}

impl Queue {
    /// A sample as -1 to 1, through PCSX2's DC-blocking filter
    /// (`spu2.cpp`'s `DCFilter`).
    fn dc_block(&mut self, sample: [i16; 2]) -> [f32; 2] {
        [0, 1].map(|c| {
            let input = f32::from(sample[c]) / f32::from(i16::MAX);
            let [last_in, last_out] = self.dc[c];
            let out = input - last_in + 0.995 * last_out;
            self.dc[c] = [input, out];
            out
        })
    }

    /// The next output frame at `step` input samples per output frame,
    /// or silence when the queue has run dry.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "sample counts and a phase between 0 and 1"
    )]
    fn next(&mut self, step: f64) -> [f32; 2] {
        if self.samples.len() < 2 {
            return [0.0; 2];
        }
        let fill = self.samples.len() as f64;
        let target = self.target as f64;
        // Nudge the rate towards keeping the queue at its target.
        let nudge = ((fill - target) / target * 0.005).clamp(-0.005, 0.005);
        let (a, b) = (self.samples[0], self.samples[1]);
        let t = self.phase as f32;
        let frame = [0, 1].map(|c| a[c] * (1.0 - t) + b[c] * t);
        self.phase += step * (1.0 + nudge);
        while self.phase >= 1.0 && self.samples.len() >= 2 {
            self.phase -= 1.0;
            self.samples.pop_front();
        }
        frame
    }
}

/// The sound card, fed from the machine.
pub(crate) struct Speaker {
    queue: Arc<Mutex<Queue>>,
    _stream: cpal::Stream,
}

impl Speaker {
    /// Opens the default output device, `volume` from 0 to 1.
    pub(crate) fn open(volume: f32) -> Result<Self> {
        use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no sound output device")?;
        let config = device
            .default_output_config()
            .context("the sound device's configuration")?;
        let format = config.sample_format();
        let config: cpal::StreamConfig = config.into();
        let channels = usize::from(config.channels.max(1));
        let step = f64::from(RATE) / f64::from(config.sample_rate);
        // A twentieth of a second of sound waits for the card.
        let target = (RATE / 20) as usize;
        let queue = Arc::new(Mutex::new(Queue {
            samples: VecDeque::with_capacity(target * 4),
            phase: 0.0,
            target,
            dc: [[0.0; 2]; 2],
        }));
        let reader = Arc::clone(&queue);
        let fill = move |out: &mut dyn FnMut(usize, f32), frames: usize| {
            let Ok(mut queue) = reader.lock() else {
                return;
            };
            for f in 0..frames {
                let [l, r] = queue.next(step);
                for c in 0..channels {
                    out(f * channels + c, volume * if c % 2 == 0 { l } else { r });
                }
            }
        };
        let error = |e| tracing::warn!("sound output: {e}");
        let stream = match format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let frames = data.len() / channels;
                    fill(&mut |i, v| data[i] = v, frames);
                },
                error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let frames = data.len() / channels;
                    fill(
                        #[allow(clippy::cast_possible_truncation, reason = "within -1 to 1")]
                        &mut |i, v| data[i] = (v * f32::from(i16::MAX)) as i16,
                        frames,
                    );
                },
                error,
                None,
            ),
            other => anyhow::bail!("sound output: samples as {other:?} are not supported"),
        }
        .context("opening the sound output")?;
        stream.play().context("starting the sound output")?;
        tracing::info!("sound: {} Hz, {channels} channels", config.sample_rate);
        Ok(Self {
            queue,
            _stream: stream,
        })
    }

    /// Hands over the machine's samples; if the card has fallen far behind
    /// (a stall), the oldest are dropped rather than played late.
    pub(crate) fn push(&self, samples: &[[i16; 2]]) {
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        for sample in samples {
            let filtered = queue.dc_block(*sample);
            queue.samples.push_back(filtered);
        }
        let limit = queue.target * 4;
        while queue.samples.len() > limit {
            queue.samples.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_resamples_and_falls_silent_when_dry() {
        let queue = |samples: Vec<[f32; 2]>, target| Queue {
            samples: samples.into(),
            phase: 0.0,
            target,
            dc: [[0.0; 2]; 2],
        };
        // Same rate, queue at its target: one sample per frame.
        let mut q = queue(
            (0..100u8).map(|i| [f32::from(i), -f32::from(i)]).collect(),
            100,
        );
        assert!(q.next(1.0)[0].abs() < 1e-6);
        let second = q.next(1.0);
        assert!((second[0] - 1.0).abs() < 1e-4 && (second[1] + 1.0).abs() < 1e-4);
        // Half the rate: two frames per sample, the second between them.
        let mut q = queue(vec![[0.0; 2], [1.0; 2], [2.0; 2]], 3);
        q.next(0.5);
        assert!((q.next(0.5)[0] - 0.5).abs() < 1e-3);
        // Dry: silence.
        assert_eq!(queue(Vec::new(), 10).next(1.0), [0.0, 0.0]);
        // A constant level fades out through the DC filter.
        let mut q = queue(Vec::new(), 10);
        let first = q.dc_block([16384, 16384]);
        let later = (0..2000).map(|_| q.dc_block([16384, 16384])).last();
        assert!(first[0] > 0.49 && later.is_some_and(|l| l[0].abs() < 0.001));
    }
}
