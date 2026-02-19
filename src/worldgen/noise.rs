use fastnoise_lite::{FastNoiseLite, NoiseType};

/// Dünner Wrapper um FastNoiseLite:
/// - deterministische Seeds
/// - fBm für sanfte Hügel
/// - separater Kanal für Cliff-Shaping
pub struct NoiseSampler {
    base: FastNoiseLite,
    cliffs: FastNoiseLite,
}

impl NoiseSampler {
    pub fn new(seed: u32) -> Self {
        let mut base = FastNoiseLite::with_seed(seed as i32);
        base.set_noise_type(Some(NoiseType::OpenSimplex2));
        // Wir skalieren Frequenzen bereits explizit in fbm2()/cliff2().
        // Daher interne FastNoise-Frequenz auf 1.0 setzen (Default ist 0.01).
        base.set_frequency(Some(1.0));

        let mut cliffs = FastNoiseLite::with_seed(seed.wrapping_add(0x9E37_79B9) as i32);
        cliffs.set_noise_type(Some(NoiseType::OpenSimplex2));
        cliffs.set_frequency(Some(1.0));

        Self { base, cliffs }
    }

    /// fBm: Summe mehrerer Oktaven der gleichen Noise-Funktion.
    /// Normalisiert grob auf [-1..1].
    pub fn fbm2(&self, x: f32, z: f32, freq: f32, octaves: u8, lacunarity: f32, gain: f32) -> f32 {
        let mut amplitude = 1.0_f32;
        let mut frequency = freq.max(0.000_001);
        let mut sum = 0.0_f32;
        let mut norm = 0.0_f32;

        for _ in 0..octaves.max(1) {
            let n = self.base.get_noise_2d(x * frequency, z * frequency);
            sum += n * amplitude;
            norm += amplitude;
            amplitude *= gain;
            frequency *= lacunarity.max(1.0);
        }

        if norm > 0.0 {
            (sum / norm).clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }

    pub fn cliff2(&self, x: f32, z: f32, freq: f32) -> f32 {
        self.cliffs
            .get_noise_2d(x * freq, z * freq)
            .clamp(-1.0, 1.0)
    }
}
