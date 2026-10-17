//! Property-style fuzzing of the frame parser with a deterministic generator:
//! arbitrary and mutated input never panics, and anything accepted is valid
//! and re-encodes to an equal frame.
use phorminx_messaging::{Frame, MAX_FRAME_BYTES, read_frame};

/// xorshift64*: small, deterministic, good enough to explore inputs.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

fn seeds() -> Vec<Vec<u8>> {
    let id = "0a1b2c3d-0000-4000-8000-000000000001";
    let key = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE";
    [
        format!(r#"{{"type":"hello","version":1,"identity_key":"{key}","display_name":"Ana"}}"#),
        format!(r#"{{"type":"message","id":"{id}","sent_at":1700000000000,"text":"olá\nmundo"}}"#),
        format!(r#"{{"type":"ack","id":"{id}"}}"#),
        r#"{"type":"goodbye"}"#.to_string(),
        r#"{"type":"invite_rejected"}"#.to_string(),
    ]
    .into_iter()
    .map(String::into_bytes)
    .collect()
}

fn check(body: &[u8]) {
    if let Ok(frame) = Frame::decode(body) {
        frame.validate().expect("accepted frames are valid");
        let encoded = frame.encode().expect("accepted frames re-encode");
        assert!(encoded.len() <= MAX_FRAME_BYTES + 4);
        assert_eq!(Frame::decode(&encoded[4..]).unwrap(), frame);
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..20_000 {
        let len = rng.below(300);
        check(&rng.bytes(len));
    }
}

#[test]
fn mutated_valid_frames_never_panic_and_stay_valid() {
    let mut rng = Rng(42);
    let seeds = seeds();
    for seed in &seeds {
        assert!(
            Frame::decode(seed).is_ok(),
            "{}",
            String::from_utf8_lossy(seed)
        );
    }
    for _ in 0..30_000 {
        let mut body = seeds[rng.below(seeds.len())].clone();
        for _ in 0..1 + rng.below(4) {
            match rng.below(4) {
                0 if !body.is_empty() => {
                    let i = rng.below(body.len());
                    body[i] = rng.next() as u8;
                }
                1 if !body.is_empty() => {
                    body.remove(rng.below(body.len()));
                }
                2 => {
                    let i = rng.below(body.len() + 1);
                    body.insert(i, br#"{}[]":,0a\u"#[rng.below(11)]);
                }
                _ => {
                    let i = rng.below(body.len() + 1);
                    body.truncate(i);
                }
            }
        }
        check(&body);
    }
}

#[tokio::test]
async fn stream_reader_survives_garbage() {
    let mut rng = Rng(7);
    for _ in 0..2_000 {
        let len = rng.below(64);
        let mut data = rng.bytes(len);
        // Bias toward small, plausible length prefixes.
        if data.len() >= 4 && rng.below(2) == 0 {
            data[..4].copy_from_slice(&(rng.below(60) as u32).to_be_bytes());
        }
        let mut reader = &data[..];
        let _ = read_frame(&mut reader).await;
    }
}
