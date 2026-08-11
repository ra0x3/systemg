#![no_main]

use libfuzzer_sys::fuzz_target;
use systemg::ipc::decode_control_frame;

fuzz_target!(|data: &[u8]| {
    let _ = decode_control_frame(data);
});
