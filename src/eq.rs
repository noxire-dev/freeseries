//! Parametric EQ: RBJ cookbook biquads, configured from AutoEq / Equalizer APO
//! `ParametricEQ.txt` syntax so the AutoEq preset database loads unmodified.

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Band {
    pub kind: Kind,
    pub fc: f64,
    pub gain_db: f64,
    pub q: f64,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub preamp_db: f64,
    pub bands: Vec<Band>,
}

/// Parses lines like `Preamp: -6.2 dB` and
/// `Filter 1: ON PK Fc 105 Hz Gain -2.5 dB Q 0.70`. Unknown lines and OFF filters are skipped.
pub fn parse(text: &str) -> Preset {
    let mut preset = Preset::default();
    for line in text.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        let after = |key: &str| {
            t.iter()
                .position(|w| w.eq_ignore_ascii_case(key))
                .and_then(|i| t.get(i + 1))
                .and_then(|v| v.parse::<f64>().ok())
        };
        if t.first().is_some_and(|w| w.eq_ignore_ascii_case("Preamp:")) {
            preset.preamp_db = t.get(1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
        } else if t.first().is_some_and(|w| w.eq_ignore_ascii_case("Filter")) && t.contains(&"ON") {
            let kind = match t.iter().find(|w| ["PK", "LSC", "HSC", "LS", "HS"].contains(w)) {
                Some(&"PK") => Kind::Peak,
                Some(&"LSC") | Some(&"LS") => Kind::LowShelf,
                Some(&"HSC") | Some(&"HS") => Kind::HighShelf,
                _ => continue,
            };
            if let (Some(fc), Some(gain_db)) = (after("Fc"), after("Gain")) {
                let q = after("Q").unwrap_or(std::f64::consts::FRAC_1_SQRT_2);
                preset.bands.push(Band { kind, fc, gain_db, q });
            }
        }
    }
    preset
}

/// Normalised biquad coefficients (a0 = 1).
#[derive(Clone, Copy)]
struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

fn coeffs(band: Band, sample_rate: f64) -> Coeffs {
    let a = 10f64.powf(band.gain_db / 40.0);
    let w0 = 2.0 * PI * band.fc / sample_rate;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * band.q);
    let (b0, b1, b2, a0, a1, a2) = match band.kind {
        Kind::Peak => (
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        ),
        Kind::LowShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) - (a - 1.0) * cos + s),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - s),
                (a + 1.0) + (a - 1.0) * cos + s,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - s,
            )
        }
        Kind::HighShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) + (a - 1.0) * cos + s),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - s),
                (a + 1.0) - (a - 1.0) * cos + s,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - s,
            )
        }
    };
    Coeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

/// Stateful EQ over interleaved f32 frames.
pub struct Eq {
    channels: usize,
    sample_rate: f64,
    preamp: f64,
    coeffs: Vec<Coeffs>,
    /// Transposed direct form II state, indexed `[band * channels + channel]`.
    state: Vec<[f64; 2]>,
}

impl Eq {
    pub fn new(preset: &Preset, sample_rate: f64, channels: usize) -> Self {
        let mut eq = Eq { channels, sample_rate, preamp: 1.0, coeffs: Vec::new(), state: Vec::new() };
        eq.retune(preset);
        eq
    }

    /// Applies new settings while audio runs. Filter state survives as long as the band count
    /// doesn't change, so dragging a band doesn't click.
    pub fn retune(&mut self, preset: &Preset) {
        self.preamp = 10f64.powf(preset.preamp_db / 20.0);
        self.coeffs = preset.bands.iter().map(|&b| coeffs(b, self.sample_rate)).collect();
        if self.state.len() != self.coeffs.len() * self.channels {
            self.state = vec![[0.0; 2]; self.coeffs.len() * self.channels];
        }
    }

    pub fn process(&mut self, interleaved: &mut [f32]) {
        for frame in interleaved.chunks_exact_mut(self.channels) {
            for (ch, sample) in frame.iter_mut().enumerate() {
                let mut x = *sample as f64 * self.preamp;
                for (i, c) in self.coeffs.iter().enumerate() {
                    let s = &mut self.state[i * self.channels + ch];
                    let y = c.b0 * x + s[0];
                    s[0] = c.b1 * x - c.a1 * y + s[1];
                    s[1] = c.b2 * x - c.a2 * y;
                    x = y;
                }
                *sample = x as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_autoeq_format() {
        let p = parse(
            "Preamp: -6.2 dB\n\
             Filter 1: ON LSC Fc 105 Hz Gain 6.5 dB Q 0.70\n\
             Filter 2: ON PK Fc 2500 Hz Gain -2.5 dB Q 1.41\n\
             Filter 3: OFF PK Fc 100 Hz Gain 1 dB Q 1\n\
             Filter 4: ON HSC Fc 10000 Hz Gain -3 dB Q 0.70\n",
        );
        assert_eq!(p.preamp_db, -6.2);
        assert_eq!(p.bands.len(), 3);
        assert_eq!(p.bands[1], Band { kind: Kind::Peak, fc: 2500.0, gain_db: -2.5, q: 1.41 });
        assert_eq!(p.bands[2].kind, Kind::HighShelf);
    }

    /// Steady-state gain of a sine through the EQ, in dB.
    fn gain_at(preset: &Preset, freq: f64) -> f64 {
        let sr = 48000.0;
        let mut eq = Eq::new(preset, sr, 1);
        let mut buf: Vec<f32> = (0..48000).map(|n| (2.0 * PI * freq * n as f64 / sr).sin() as f32).collect();
        eq.process(&mut buf);
        let peak = buf[24000..].iter().fold(0f32, |m, s| m.max(s.abs()));
        20.0 * (peak as f64).log10()
    }

    #[test]
    fn filters_hit_their_target_gain() {
        let band = |kind, fc, gain_db| Preset { preamp_db: 0.0, bands: vec![Band { kind, fc, gain_db, q: 1.0 }] };
        assert!((gain_at(&band(Kind::Peak, 1000.0, 6.0), 1000.0) - 6.0).abs() < 0.05);
        assert!(gain_at(&band(Kind::Peak, 1000.0, 6.0), 60.0).abs() < 0.1);
        assert!((gain_at(&band(Kind::LowShelf, 200.0, -4.0), 20.0) + 4.0).abs() < 0.1);
        assert!((gain_at(&band(Kind::HighShelf, 4000.0, 5.0), 18000.0) - 5.0).abs() < 0.1);
    }
}
