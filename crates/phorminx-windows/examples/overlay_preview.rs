//! Shows the status pill with live text, then a result with a note and the
//! Expand button, so its layout can be checked by eye:
//! `cargo run -p phorminx-windows --example overlay_preview`.
//! Set `PHORMINX_OVERLAY_HOLD_MS` to keep the long live text on screen longer,
//! for example while capturing the README image (docs/features/CAPTURE.md).

use std::thread::sleep;
use std::time::Duration;

use phorminx_windows::{OverlayStatus, StatusOverlay};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let overlay = StatusOverlay::start()?;
    overlay.set(OverlayStatus::Listening)?;
    sleep(Duration::from_millis(800));
    overlay.set_live_text(Some("so I was thinking"))?;
    sleep(Duration::from_millis(1_200));
    overlay.set_live_text(Some(
        "so I was thinking that maybe we should change how the onboarding works because right \
         now people open the app and they don't know what to do so maybe we show them one screen",
    ))?;
    let hold = std::env::var("PHORMINX_OVERLAY_HOLD_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2_500);
    sleep(Duration::from_millis(hold));
    overlay.set(OverlayStatus::Transcribing)?;
    sleep(Duration::from_millis(1_500));
    overlay.set_live_text(None)?;
    overlay.set_note(Some("Unformatted: the local model did not answer in time"))?;
    overlay.offer_expand(true)?;
    overlay.set(OverlayStatus::Inserted)?;
    sleep(Duration::from_millis(4_000));
    for event in overlay.events().try_iter() {
        println!("{event:?}");
    }
    overlay.shutdown()?;
    Ok(())
}
