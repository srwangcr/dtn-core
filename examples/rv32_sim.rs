#![cfg_attr(target_arch = "riscv32", no_std)]
#![cfg_attr(target_arch = "riscv32", no_main)]

#[cfg(target_arch = "riscv32")]
use core::panic::PanicInfo;
#[cfg(target_arch = "riscv32")]
use dtn_core::parser::extension_block::{parse_canonical_block, BlockType};
#[cfg(target_arch = "riscv32")]
use dtn_core::parser::primary_block::parse_primary_block;
#[cfg(target_arch = "riscv32")]
use dtn_core::processor::state_machine::{BundleProcessor, ProcessStatus};
#[cfg(target_arch = "riscv32")]
use dtn_core::storage::ring_buffer::{Consumer, LockFreeRingBuffer, Producer};
#[cfg(target_arch = "riscv32")]
use dtn_core::telemetry::metrics::SystemMetrics;
#[cfg(target_arch = "riscv32")]
use riscv_rt::entry;

#[cfg(target_arch = "riscv32")]
const UART0: usize = 0x1000_0000;

#[cfg(target_arch = "riscv32")]
const UART_RBR_THR: usize = 0;
#[cfg(target_arch = "riscv32")]
const UART_LSR: usize = 5;
#[cfg(target_arch = "riscv32")]
const UART_LSR_DATA_READY: u8 = 1 << 0;
#[cfg(target_arch = "riscv32")]
const UART_LSR_THR_EMPTY: u8 = 1 << 5;

#[cfg(target_arch = "riscv32")]
const RX_RING_SIZE: usize = 1024;
#[cfg(target_arch = "riscv32")]
const FRAME_BUFFER_SIZE: usize = 2048;

#[cfg(target_arch = "riscv32")]
static mut RX_RING: LockFreeRingBuffer<RX_RING_SIZE> = LockFreeRingBuffer::new();

#[cfg(target_arch = "riscv32")]
static RX_METRICS: SystemMetrics = SystemMetrics::new();

#[cfg(target_arch = "riscv32")]
#[derive(Clone, Copy)]
enum FrameProgress {
    NeedMore,
    Accepted,
    Expired,
    Malformed(&'static str),
}

#[cfg(target_arch = "riscv32")]
fn uart_read(reg: usize) -> u8 {
    unsafe { core::ptr::read_volatile((UART0 + reg) as *const u8) }
}

#[cfg(target_arch = "riscv32")]
fn uart_write(reg: usize, byte: u8) {
    unsafe { core::ptr::write_volatile((UART0 + reg) as *mut u8, byte) }
}

#[cfg(target_arch = "riscv32")]
fn uart_read_byte() -> Option<u8> {
    if uart_read(UART_LSR) & UART_LSR_DATA_READY != 0 {
        Some(uart_read(UART_RBR_THR))
    } else {
        None
    }
}

#[cfg(target_arch = "riscv32")]
fn putchar(byte: u8) {
    while uart_read(UART_LSR) & UART_LSR_THR_EMPTY == 0 {
        core::hint::spin_loop();
    }
    uart_write(UART_RBR_THR, byte);
}

#[cfg(target_arch = "riscv32")]
fn puts(message: &[u8]) {
    for &byte in message {
        putchar(byte);
    }
}

#[cfg(target_arch = "riscv32")]
fn put_u64(mut value: u64) {
    let mut digits = [0u8; 20];
    let mut len = 0;

    if value == 0 {
        putchar(b'0');
        return;
    }

    while value != 0 {
        digits[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
    }

    while len != 0 {
        len -= 1;
        putchar(digits[len]);
    }
}

#[cfg(target_arch = "riscv32")]
fn report(message: &[u8], bytes: usize) {
    puts(message);
    put_u64(bytes as u64);
    puts(b" bytes\r\n");
}

#[cfg(target_arch = "riscv32")]
fn report_metrics(metrics: &SystemMetrics) {
    let snapshot = metrics.snapshot();
    puts(b"METRICS processed=");
    put_u64(snapshot.packets_processed);
    puts(b" dropped=");
    put_u64(snapshot.packets_dropped);
    puts(b" avg_latency_ns=");
    put_u64(snapshot.latency_avg_ns);
    puts(b"\r\n");
}

#[cfg(target_arch = "riscv32")]
fn is_incomplete_error(error: &str) -> bool {
    matches!(
        error,
        "empty buffer"
            | "buffer empty"
            | "empty"
            | "truncated cbor"
            | "short buf for u8"
            | "short buf for u16"
            | "short buf for u32"
            | "short buf for u64"
            | "short for u8 len"
            | "short for u16 len"
            | "short for u32 len"
            | "short for u64 len"
            | "slice len"
            | "string extends past buffer"
            | "unexpected end while reading map key"
            | "unexpected end while reading map value"
            | "unexpected end in array ts_sec"
            | "unexpected end in array ts_seq"
            | "unexpected end skipping array extra"
            | "truncated block specific data"
    )
}

#[cfg(target_arch = "riscv32")]
fn inspect_frame(frame: &[u8], metrics: &SystemMetrics) -> FrameProgress {
    let primary = match parse_primary_block(frame) {
        Ok(primary) => primary,
        Err(error) if is_incomplete_error(error) => return FrameProgress::NeedMore,
        Err(error) => return FrameProgress::Malformed(error),
    };

    let mut cursor = primary.raw.len();
    while cursor < frame.len() {
        match parse_canonical_block(&frame[cursor..]) {
            Ok((block, consumed)) => {
                cursor += consumed;
                if block.block_type == BlockType::Payload {
                    return match BundleProcessor::process_bundle(frame, 0, Some(metrics)).status {
                        ProcessStatus::Accepted => FrameProgress::Accepted,
                        ProcessStatus::Expired => FrameProgress::Expired,
                        ProcessStatus::RejectedMalformed => {
                            FrameProgress::Malformed("bundle rejected by processor")
                        }
                    };
                }
            }
            Err(error) if is_incomplete_error(error) => return FrameProgress::NeedMore,
            Err(error) => return FrameProgress::Malformed(error),
        }
    }

    FrameProgress::NeedMore
}

#[cfg(target_arch = "riscv32")]
fn consume_rx(producer: &mut Producer<'_, RX_RING_SIZE>, consumer: &mut Consumer<'_, RX_RING_SIZE>) -> ! {
    let mut frame = [0u8; FRAME_BUFFER_SIZE];
    let mut frame_len = 0usize;
    let mut discarded_noise = 0usize;

    loop {
        while let Some(byte) = uart_read_byte() {
            if producer.push_byte(byte).is_err() {
                puts(b"UART RX BUFFER FULL\r\n");
            }
        }

        while let Some(byte) = consumer.pop_byte() {
            if frame_len == 0 && byte != 0xA6 {
                discarded_noise += 1;
                continue;
            }

            if discarded_noise != 0 {
                report(b"BPv7 noise discarded:", discarded_noise);
                discarded_noise = 0;
            }

            if frame_len != 0 && byte == 0xA6 {
                puts(b"BPv7 stream resynchronized\r\n");
                frame_len = 0;
            }

            if frame_len == FRAME_BUFFER_SIZE {
                puts(b"BPv7 FRAME TOO LARGE\r\n");
                frame_len = 0;
            }

            frame[frame_len] = byte;
            frame_len += 1;

            match inspect_frame(&frame[..frame_len], &RX_METRICS) {
                FrameProgress::NeedMore => {}
                FrameProgress::Accepted => {
                    report(b"BPv7 bundle accepted:", frame_len);
                    report_metrics(&RX_METRICS);
                    frame_len = 0;
                }
                FrameProgress::Expired => {
                    report(b"BPv7 bundle expired:", frame_len);
                    report_metrics(&RX_METRICS);
                    frame_len = 0;
                }
                FrameProgress::Malformed(error) => {
                    if error != "bundle rejected by processor" {
                        RX_METRICS.record_packet();
                        RX_METRICS.record_drop();
                    }
                    puts(b"BPv7 decode failure: ");
                    puts(error.as_bytes());
                    puts(b" (" );
                    put_u64(frame_len as u64);
                    puts(b" bytes)\r\n");
                    report_metrics(&RX_METRICS);
                    frame_len = 0;
                }
            }
        }

        core::hint::spin_loop();
    }
}

#[cfg(target_arch = "riscv32")]
#[entry]
fn main() -> ! {
    puts(b"DTN-Core RV32 UART RX ready\r\n");

    let (mut producer, mut consumer) = unsafe {
        (&mut *core::ptr::addr_of_mut!(RX_RING)).split()
    };
    consume_rx(&mut producer, &mut consumer);
}

#[cfg(target_arch = "riscv32")]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(target_arch = "riscv32"))]
fn main() {}