#![no_main]

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use skattr_mailbox::codec::MailboxFrameCodec;
use tokio_util::codec::Decoder;

fuzz_target!(|data: &[u8]| {
    let mut codec = MailboxFrameCodec::new();
    let mut buf = BytesMut::from(data);
    // Run decode in a loop until either it returns Ok(None) (need
    // more bytes), errors, or empties the buffer. Errors are expected
    // — we only assert no panic.
    loop {
        match codec.decode(&mut buf) {
            Ok(Some(_frame)) => {
                if buf.is_empty() {
                    break;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
});
