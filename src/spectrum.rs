//! Spectrum of what is playing, for the analyser behind the EQ curve.
//!
//! The audio callback only copies samples into a ring (`Tap::push`). The FFT runs on whoever asks
//! for a spectrum, never on the audio thread.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

/// 8192 points at 48 kHz: 5.9 Hz per bin, enough to tell 40 Hz from 50 Hz, ~170 ms of audio.
const FFT_SIZE: usize = 8192;
const FLOOR_DB: f32 = -120.0;
/// Loopback capture delivers nothing while the PC is silent. Audio older than this is over.
const STALE: Duration = Duration::from_millis(250);
pub const F_MIN: f32 = 20.0;
pub const F_MAX: f32 = 20_000.0;

/// The most recent `FFT_SIZE` mono samples.
pub struct Tap {
    ring: Vec<f32>,
    next: usize,
    sample_rate: f32,
    last_push: Option<Instant>,
}

impl Default for Tap {
    fn default() -> Self {
        Tap { ring: vec![0.0; FFT_SIZE], next: 0, sample_rate: 48_000.0, last_push: None }
    }
}

impl Tap {
    /// Mixes interleaved frames down to mono and appends them. Cheap: called on the audio thread.
    pub fn push(&mut self, interleaved: &[f32], channels: usize, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.last_push = Some(Instant::now());
        for frame in interleaved.chunks_exact(channels) {
            self.ring[self.next] = frame.iter().sum::<f32>() / channels as f32;
            self.next = (self.next + 1) % FFT_SIZE;
        }
    }

    /// Level in dBFS of `bands` log-spaced bands from 20 Hz to 20 kHz. A full-scale sine reads 0 dB.
    pub fn spectrum(&self, bands: usize) -> Vec<f32> {
        if self.last_push.is_none_or(|at| at.elapsed() > STALE) {
            return vec![FLOOR_DB; bands];
        }
        // Oldest sample first, Hann-windowed.
        let mut re: Vec<f32> = (0..FFT_SIZE)
            .map(|i| self.ring[(self.next + i) % FFT_SIZE] * (0.5 - 0.5 * (2.0 * PI * i as f32 / FFT_SIZE as f32).cos()))
            .collect();
        let mut im = vec![0.0; FFT_SIZE];
        fft(&mut re, &mut im);
        // /(N/2) turns a bin into a sine's amplitude; the Hann window halves it again.
        let scale = 4.0 / FFT_SIZE as f32;
        let magnitude: Vec<f32> = (0..FFT_SIZE / 2).map(|k| re[k].hypot(im[k]) * scale).collect();

        let bin_hz = self.sample_rate / FFT_SIZE as f32;
        let edge = |i: usize| F_MIN * (F_MAX / F_MIN).powf(i as f32 / bands as f32);
        (0..bands)
            .map(|b| {
                let (lo, hi) = (edge(b) / bin_hz, edge(b + 1) / bin_hz);
                let (first, last) = (lo.ceil() as usize, (hi.floor() as usize).min(magnitude.len() - 1));
                let level = if first <= last {
                    magnitude[first..=last].iter().cloned().fold(0.0, f32::max) // the loudest bin in the band
                } else {
                    // Narrower than one bin (the lowest bands): read between the two nearest bins.
                    let centre = ((lo + hi) / 2.0).min(magnitude.len() as f32 - 2.0);
                    let k = centre.floor() as usize;
                    magnitude[k] + (magnitude[k + 1] - magnitude[k]) * (centre - k as f32)
                };
                (20.0 * level.max(1e-9).log10()).max(FLOOR_DB)
            })
            .collect()
    }
}

/// In-place radix-2 FFT. `re.len()` must be a power of two.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    // Bit-reversal permutation.
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * PI / len as f32;
        let (step_re, step_im) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut w_re, mut w_im) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (t_re, t_im) = (re[b] * w_re - im[b] * w_im, re[b] * w_im + im[b] * w_re);
                (re[b], im[b]) = (re[a] - t_re, im[a] - t_im);
                (re[a], im[a]) = (re[a] + t_re, im[a] + t_im);
                (w_re, w_im) = (w_re * step_re - w_im * step_im, w_re * step_im + w_im * step_re);
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band_of(freq: f32, bands: usize) -> usize {
        ((freq / F_MIN).ln() / (F_MAX / F_MIN).ln() * bands as f32) as usize
    }

    #[test]
    fn a_sine_shows_up_at_its_level_in_its_band_and_nowhere_else() {
        let mut tap = Tap::default();
        // Stereo, half scale, 1 kHz. Pushed in uneven blocks like a real callback would.
        let samples: Vec<f32> = (0..FFT_SIZE * 2).flat_map(|n| [0.5 * (2.0 * PI * 1000.0 * n as f32 / 48_000.0).sin(); 2]).collect();
        for block in samples.chunks(2 * 441) {
            tap.push(block, 2, 48_000.0);
        }
        let spectrum = tap.spectrum(96);
        let peak = band_of(1000.0, 96);
        assert!((spectrum[peak] + 6.0).abs() < 1.5, "1 kHz at half scale should read about -6 dBFS, got {}", spectrum[peak]);
        assert!(spectrum[band_of(100.0, 96)] < -70.0 && spectrum[band_of(10_000.0, 96)] < -70.0, "energy leaked far from the tone");
    }

    #[test]
    fn the_lowest_bands_read_a_bass_tone_too() {
        let mut tap = Tap::default();
        let samples: Vec<f32> = (0..FFT_SIZE * 2).map(|n| (2.0 * PI * 40.0 * n as f32 / 48_000.0).sin()).collect();
        tap.push(&samples, 1, 48_000.0);
        let spectrum = tap.spectrum(96);
        let loudest = spectrum.iter().cloned().fold(FLOOR_DB, f32::max);
        assert!(loudest > -6.0, "a full-scale 40 Hz tone should be near 0 dBFS somewhere, loudest band is {loudest}");
        assert!((0..96).max_by(|a, b| spectrum[*a].total_cmp(&spectrum[*b])).unwrap().abs_diff(band_of(40.0, 96)) <= 1);
    }
}
