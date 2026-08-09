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

pub fn input_buffer_size(supported: &SupportedBufferSize) -> BufferSize {
    input_buffer_size_for(supported, is_wsl())
}

fn input_buffer_size_for(supported: &SupportedBufferSize, is_wsl: bool) -> BufferSize {
    if is_wsl {
        return BufferSize::Default;
    }

    match supported {
        SupportedBufferSize::Range { min, max } => {
            BufferSize::Fixed(NATIVE_LOW_LATENCY_FRAMES.clamp(*min, *max))
        }
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

pub fn monitor_prefill_ms() -> usize {
    fs::read_to_string("/proc/sys/kernel/osrelease").map_or(NATIVE_MONITOR_PREFILL_MS, |release| {
        monitor_prefill_ms_for_os_release(&release)
    })
}

fn is_wsl() -> bool {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"))
}

fn monitor_prefill_ms_for_os_release(os_release: &str) -> usize {
    if os_release.to_ascii_lowercase().contains("microsoft") {
        WSL_MONITOR_PREFILL_MS
    } else {
        NATIVE_MONITOR_PREFILL_MS
    }
}

#[cfg(test)]
mod tests {
    use super::{input_buffer_size_for, monitor_prefill_ms_for_os_release};
    use rodio::cpal::{BufferSize, SupportedBufferSize};

    #[test]
    fn native_linux_uses_one_small_monitor_prefill() {
        assert_eq!(monitor_prefill_ms_for_os_release("7.0.12-arch1-1"), 5);
    }

    #[test]
    fn wsl_keeps_the_stable_monitor_prefill() {
        assert_eq!(
            monitor_prefill_ms_for_os_release("6.6.87.2-microsoft-standard-WSL2"),
            100
        );
    }

    #[test]
    fn native_input_buffer_is_clamped_to_device_range() {
        assert_eq!(
            input_buffer_size_for(&SupportedBufferSize::Range { min: 32, max: 256 }, false),
            BufferSize::Fixed(256)
        );
        assert_eq!(
            input_buffer_size_for(
                &SupportedBufferSize::Range {
                    min: 32,
                    max: 2_048
                },
                false
            ),
            BufferSize::Fixed(512)
        );
    }

    #[test]
    fn wsl_leaves_input_buffer_to_the_audio_server() {
        assert_eq!(
            input_buffer_size_for(
                &SupportedBufferSize::Range {
                    min: 32,
                    max: 2_048
                },
                true
            ),
            BufferSize::Default
        );
    }
}
