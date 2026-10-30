//! Soft reminder chimes: a few decaying sine tones, built as an in-memory WAV
//! on first use (~50 KB each) and played asynchronously via `PlaySound`. No separate sound
//! files. Each event has its own tones so they can be told apart without looking at the screen.

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

const RATE: u32 = 22_050;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chime {
    /// Prayer time: E5 then B5, rising and calm.
    Prayer,
    /// Focus or timer finished: C5, E5, G5, rising and bright.
    Done,
    /// Break over: G5 then C5, falling, "back to work".
    Back,
}

impl Chime {
    /// (frequency in Hz, start in seconds).
    fn notes(self) -> &'static [(f32, f32)] {
        match self {
            Chime::Prayer => &[(659.25, 0.0), (987.77, 0.22)],
            Chime::Done => &[(523.25, 0.0), (659.25, 0.14), (783.99, 0.28)],
            Chime::Back => &[(783.99, 0.0), (523.25, 0.2)],
        }
    }
}

/// 16-bit mono WAV of the tones in `notes`, each fading out slowly.
fn wav(notes: &[(f32, f32)]) -> Vec<u8> {
    let len = 1.3_f32;
    let n = (RATE as f32 * len) as usize;
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let mut v = 0.0;
        for &(freq, start) in notes {
            let dt = t - start;
            if dt >= 0.0 {
                // 8 ms attack so it doesn't "click", then exponential decay.
                let env = (dt / 0.008).min(1.0) * (-dt * 4.0).exp();
                v += (std::f32::consts::TAU * freq * dt).sin() * env * 0.16;
            }
        }
        samples.push((v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
    }
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Plays the sound without waiting for it to finish. `false` if Windows refuses to play it.
pub fn play(kind: Chime) -> bool {
    static SOUNDS: [OnceLock<Vec<u8>>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let i = match kind {
        Chime::Prayer => 0,
        Chime::Done => 1,
        Chime::Back => 2,
    };
    let sound = SOUNDS[i].get_or_init(|| wav(kind.notes()));
    unsafe {
        // With SND_MEMORY, the sound's "name" is a pointer to the WAV contents; the data is static, so it stays alive
        // while playing.
        PlaySoundW(PCWSTR(sound.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT).as_bool()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header_matches_data() {
        let w = super::wav(super::Chime::Done.notes());
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize, w.len() - 8);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize, w.len() - 44);
    }
}
