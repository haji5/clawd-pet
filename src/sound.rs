//! The optional attention chime (PLAN.md §8 item 10): when to ring it, and
//! the sound itself, built here so no audio file has to ship. Pure.

use crate::tracker::{Attention, Mode, Ms};

/// A burst of prompts rings once.
pub const QUIET_FOR: Ms = 10_000;

/// A session has just started waiting on the user. The gentle "your turn"
/// doesn't ring, and neither does a waiting prompt turning impatient.
pub fn rings(before: Mode, after: Mode) -> bool {
    let waiting = |m| matches!(m, Mode::Attention(Attention::Permission | Attention::WaitingForInput | Attention::Impatient));
    waiting(after) && !waiting(before)
}

const RATE: u32 = 11_025;

/// Two soft notes going up, as an 8-bit mono WAV.
pub fn chime() -> Vec<u8> {
    let mut samples = Vec::new();
    for (freq, ms) in [(880.0_f64, 90_u32), (1175.0, 130)] {
        let n = RATE * ms / 1000;
        for i in 0..n {
            let t = i as f64 / RATE as f64;
            let fade = 1.0 - i as f64 / n as f64;
            let attack = (i as f64 / 40.0).min(1.0);
            let wave = (t * freq * std::f64::consts::TAU).sin();
            samples.push((128.0 + 36.0 * attack * fade * wave) as u8);
        }
    }
    let size = samples.len() as u32;
    let mut wav = Vec::with_capacity(44 + samples.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes()); // bytes per second
    wav.extend_from_slice(&1u16.to_le_bytes()); // bytes per sample
    wav.extend_from_slice(&8u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&size.to_le_bytes());
    wav.extend_from_slice(&samples);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::Work;

    const PERMISSION: Mode = Mode::Attention(Attention::Permission);
    const QUESTION: Mode = Mode::Attention(Attention::WaitingForInput);
    const IMPATIENT: Mode = Mode::Attention(Attention::Impatient);
    const YOUR_TURN: Mode = Mode::Attention(Attention::YourTurn);
    const WORKING: Mode = Mode::Working(Work::Thinking);

    #[test]
    fn it_rings_when_someone_starts_waiting() {
        assert!(rings(WORKING, PERMISSION));
        assert!(rings(Mode::Idle, QUESTION));
        assert!(rings(YOUR_TURN, PERMISSION));
    }

    #[test]
    fn it_doesnt_ring_for_anything_else() {
        assert!(!rings(PERMISSION, IMPATIENT), "the same prompt");
        assert!(!rings(IMPATIENT, PERMISSION));
        assert!(!rings(PERMISSION, QUESTION));
        assert!(!rings(WORKING, YOUR_TURN));
        assert!(!rings(PERMISSION, WORKING));
    }

    #[test]
    fn the_chime_is_a_well_formed_little_wav() {
        let w = chime();
        assert_eq!((&w[0..4], &w[8..16], &w[36..40]), (&b"RIFF"[..], &b"WAVEfmt "[..], &b"data"[..]));
        let riff = u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize;
        let data = u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize;
        assert_eq!((riff + 8, data + 44), (w.len(), w.len()));
        assert!(w.len() < 4_096, "{} bytes", w.len());
        assert!(w[44..].iter().all(|s| (88..=168).contains(s)), "quiet, and never clips");
    }
}
