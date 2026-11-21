#![no_main]
use libfuzzer_sys::fuzz_target;
use shisa_core::config::UserConfig;

// The file is root-written, but a mistake in it must still be a parse error
// and not a panic in the daemon.
fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = UserConfig::parse(text);
    }
});
