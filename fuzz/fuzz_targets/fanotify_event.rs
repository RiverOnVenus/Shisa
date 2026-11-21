#![no_main]
use libfuzzer_sys::fuzz_target;

// A record longer than its buffer or an unknown version must be an error,
// never a read past the buffer. The decoder runs while exec waits on the
// kernel, so a panic here loses the gate.
fuzz_target!(|data: &[u8]| {
    let _ = shisad::gate::fuzz_parse_event(data);
});
