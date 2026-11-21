#![no_main]
use libfuzzer_sys::fuzz_target;

// Desktop entries come from packages and the user's data directories. The
// quoting and escape handling in Exec must not panic on any input.
fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = shisa_core::desktop::split_exec(text);
    }
});
