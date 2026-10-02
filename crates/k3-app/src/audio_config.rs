use std::fs;

use rodio::cpal::{BufferSize, SupportedBufferSize};

const NATIVE_LOW_LATENCY_FRAMES: u32 = 512;
const NATIVE_MONITOR_PREFILL_MS: usize = 5;
const WSL_MONITOR_PREFILL_MS: usize = 100;

pub fn output_buffer_size() -> Option<BufferSize> {
    if is_wsl() {
        None
    } else {
        Some(BufferSize::Fixed(NATIVE_LOW_LATENCY_FRAMES))
    }
}

pub(crate) fn input_buffer_size(supported: &SupportedBufferSize) -> BufferSize {
    if is_wsl() {
        return BufferSize::Default;
    }

    match supported {
        SupportedBufferSize::Range { min, max } => {
            BufferSize::Fixed(NATIVE_LOW_LATENCY_FRAMES.clamp(*min, *max))
        }
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

pub(crate) fn monitor_prefill_ms() -> usize {
    if is_wsl() {
        WSL_MONITOR_PREFILL_MS
    } else {
        NATIVE_MONITOR_PREFILL_MS
    }
}

fn is_wsl() -> bool {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"))
}
