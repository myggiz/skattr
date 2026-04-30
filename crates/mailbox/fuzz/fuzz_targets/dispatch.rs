#![no_main]

use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;

use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;

fuzz_target!(|data: &[u8]| {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let store = Arc::new(Store::in_memory().unwrap());
        let mb = MailboxServer::new(store, Policy::recommended());
        let handle = tokio::spawn(async move { mb.accept_loop(server).await });

        // Push the fuzzer's bytes wholesale; the server's loop will
        // consume what it can, write replies, and (if asked nicely)
        // hit a typed error. We're hunting panics, not protocol
        // adherence.
        let _ = client.write_all(data).await;
        drop(client);
        let _ = handle.await;
    });
});
