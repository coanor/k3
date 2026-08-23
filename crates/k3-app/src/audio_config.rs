use std::fs;

use rodio::cpal::BufferSize;

const NATIVE_LOW_LATENCY_FRAMES: u32 = 512;

pub fn output_buffer_size() -> Option<BufferSize> {
    if is_wsl() {
        None
    } else {
        Some(BufferSize::Fixed(NATIVE_LOW_LATENCY_FRAMES))
    }
}

fn is_wsl() -> bool {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"))
}
