//! Minimal example: plays a hardcoded file for 10 seconds, printing events.
//!
//! Usage:
//! ```sh
//! cargo run --example play_file
//! ```

use musico_playback::{PlaybackEngine, SongInfo};
use std::thread;
use std::time::Duration;

fn main() {
    // Replace with an actual audio file path on your system.
    let file_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "./test_audio_file.mp3".to_string());

    println!("musico_playback example — playing: {file_path}");

    let engine = PlaybackEngine::new().expect("Failed to create playback engine");

    let song = SongInfo {
        id: "example-001".into(),
        file_path,
        title: "Test Song".into(),
        artist: "Test Artist".into(),
        album: "Test Album".into(),
        duration_secs: 0.0, // decoder will fill this
        cover_art: None,
    };

    engine.play(song).expect("Failed to start playback");

    // Poll events for 10 seconds.
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(250));

        let events = engine.poll_events();
        for event in &events {
            println!("[EVENT] {event:?}");
        }

        let state = engine.state();
        println!(
            "  status={:?}  pos={:.1}s  vol={:.0}%",
            state.status,
            state.position_secs,
            state.volume * 100.0,
        );
    }

    println!("Stopping...");
    engine.stop().expect("Failed to stop");
    thread::sleep(Duration::from_millis(500));
    println!("Done.");
}
