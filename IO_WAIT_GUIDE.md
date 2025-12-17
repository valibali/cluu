# Generic I/O Wait Queue System - Developer Guide

This document explains how to implement blocking I/O for any hardware device in the CLUU kernel.

## Overview

The I/O wait queue system provides a standard way for device drivers to block threads until hardware events occur. This eliminates busy-waiting and ensures blocked threads consume 0% CPU.

## Architecture

```
┌─────────────┐         ┌──────────────┐         ┌──────────┐
│   Thread    │         │  I/O Channel │         │   ISR    │
│  (driver)   │────────▶│  Wait Queue  │◀────────│(hardware)│
└─────────────┘         └──────────────┘         └──────────┘
     │                         │                       │
     │ wait_for_io()          │                       │
     ├───────────────────────▶│                       │
     │                         │  (thread blocked)    │
     │                         │                       │
     │                         │    wake_io_waiters() │
     │                         │◀──────────────────────┤
     │◀────────────────────────┤                       │
     │   (thread wakes up)     │                       │
```

## Adding Blocking I/O to Your Driver

### Step 1: Choose or Add an I/O Channel

First, determine if your device needs a new `IoChannel` variant:

```rust
// In kernel/src/scheduler/io_wait.rs
pub enum IoChannel {
    Keyboard,
    Serial(u8),      // Existing channels
    Timer,
    Disk(u8),
    Network(u8),

    // Add your device here:
    MyDevice(u8),    // Example: custom device with ID
}
```

### Step 2: Implement Blocking Read in Your Driver

```rust
// Example: Serial port driver

use crate::scheduler::{IoChannel, wait_for_io, wake_io_waiters};

/// Read a byte from serial port (blocking)
pub fn serial_read_blocking(port: u8) -> u8 {
    loop {
        // Try to read from buffer first
        if let Some(byte) = try_read_buffer(port) {
            return byte;
        }

        // Buffer empty - block until interrupt arrives
        if !buffer_is_empty(port) {
            continue; // Race condition check
        }

        // Block on serial I/O channel
        wait_for_io(IoChannel::Serial(port));

        // When we wake up, data has arrived
    }
}
```

### Step 3: Wake Waiters in Your ISR

```rust
// In your interrupt handler

pub fn serial_interrupt_handler() {
    // Read data from hardware port
    let port = 0; // COM1
    let byte = unsafe { read_serial_port(port) };

    // Push to ring buffer
    buffer_push(port, byte);

    // Wake all threads waiting for this serial port
    wake_io_waiters(IoChannel::Serial(port));

    // Send EOI
    // ...
}
```

### Step 4: Register Your ISR in IDT

```rust
// In kernel/src/arch/x86_64/idt.rs

idt[36].set_handler_fn(serial_interrupt_handler); // IRQ 4 - Serial COM1
```

## Complete Example: Network Driver

Here's a complete example of adding blocking I/O to a hypothetical network driver:

```rust
// kernel/src/drivers/network/e1000.rs

use crate::scheduler::{IoChannel, wait_for_io, wake_io_waiters};
use core::sync::atomic::{AtomicUsize, Ordering};

const BUFFER_SIZE: usize = 256;

// Ring buffer for received packets
static RX_BUFFER: [AtomicU32; BUFFER_SIZE] = /* ... */;
static RX_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Read a packet from network (blocking)
pub fn net_recv_blocking(nic_id: u8) -> Packet {
    loop {
        // Try to read from buffer
        if let Some(packet) = try_pop_packet() {
            return packet;
        }

        // Buffer empty - block until packet arrives
        if !rx_buffer_empty() {
            continue;
        }

        // Block on network channel
        wait_for_io(IoChannel::Network(nic_id));
    }
}

/// Network interrupt handler (called by IRQ)
pub fn e1000_interrupt_handler() {
    let nic_id = 0;

    // Read packet from NIC hardware
    let packet = read_nic_rx_ring();

    // Store in buffer
    buffer_push(packet);

    // Wake threads waiting for network I/O
    wake_io_waiters(IoChannel::Network(nic_id));

    // Acknowledge interrupt
    ack_nic_interrupt();
}
```

## Best Practices

### 1. Always Check Buffer First
```rust
// CORRECT: Check buffer before blocking
if let Some(data) = try_read_buffer() {
    return data;
}
wait_for_io(channel);

// WRONG: Don't block if data might be available
wait_for_io(channel);
return read_buffer(); // What if buffer is still empty?
```

### 2. Handle Race Conditions
```rust
loop {
    if let Some(data) = try_read_buffer() {
        return data;
    }

    // Double-check before blocking (data might have arrived)
    if !buffer_is_empty() {
        continue;
    }

    wait_for_io(channel);
}
```

### 3. Keep ISR Minimal
```rust
// CORRECT: Minimal ISR
pub fn device_isr() {
    let data = read_hardware();
    buffer_push(data);
    wake_io_waiters(channel);
}

// WRONG: Heavy processing in ISR
pub fn device_isr() {
    let data = read_hardware();
    process_data(data);           // Too slow!
    update_statistics(data);      // Too slow!
    notify_multiple_subsystems(); // Too slow!
    wake_io_waiters(channel);
}
```

### 4. Use Atomic Ring Buffers in ISR
```rust
// ISRs can't use mutexes - use atomics
static RX_BUFFER: [AtomicU32; SIZE] = /* ... */;
static RX_READ: AtomicUsize = AtomicUsize::new(0);
static RX_WRITE: AtomicUsize = AtomicUsize::new(0);
```

## Testing Your Implementation

1. **Compile and run:**
   ```bash
   make qemu
   ```

2. **Check CPU usage with `ps`:**
   - Your driver thread should show ~0-1% CPU when idle
   - Idle thread should show ~99% CPU

3. **Verify blocking:**
   - Add debug logging in `wait_for_io()` and `wake_io_waiters()`
   - Confirm thread blocks before event and wakes after

4. **Test latency:**
   - Measure time between hardware event and thread wake-up
   - Should be <1ms (one scheduler tick)

## Troubleshooting

### Thread never wakes up
- Check ISR is registered in IDT
- Verify `wake_io_waiters()` is called with correct channel
- Ensure interrupts are enabled (`sti`)

### High CPU usage despite blocking
- Verify thread isn't in tight loop before `wait_for_io()`
- Check buffer isn't constantly full (blocking never happens)
- Ensure `wait_for_io()` is actually called

### Race conditions / lost wakeups
- Always double-check buffer before blocking
- Use atomic operations for buffer management
- Keep critical sections short

## API Reference

### `wait_for_io(channel: IoChannel)`
Blocks the current thread until an I/O event occurs on the specified channel.
- **Thread context:** Must be called from thread (not ISR)
- **Blocking:** Yes - thread consumes 0% CPU while waiting
- **IRQ-safe:** No

### `wake_io_waiters(channel: IoChannel)`
Wakes all threads blocked on the specified channel.
- **Thread context:** Can be called from anywhere, including ISR
- **Blocking:** No
- **IRQ-safe:** Yes

### `IoChannel` enum
Identifies different I/O event sources. Add new variants for new devices.

## See Also

- `kernel/src/scheduler/io_wait.rs` - Implementation
- `kernel/src/drivers/input/keyboard.rs` - Reference example
- `kernel/src/scheduler/mod.rs` - Thread blocking primitives
