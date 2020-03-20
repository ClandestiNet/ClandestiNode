// Copyright (c) 2019-2020, MASQ (https://masq.ai) and/or its affiliates. All rights reserved.

use crate::utils::DaemonProcess;
use crate::utils::MasqProcess;
use std::thread;
use std::time::Duration;

mod utils;

#[test]
fn handles_interactive_mode_integration() {
    let daemon_handle = DaemonProcess::new().start(5333);
    thread::sleep(Duration::from_millis(500));
    let mut masq_handle = MasqProcess::new().start_interactive();

    masq_handle.type_command ("setup --neighborhood-mode zero-hop");
    masq_handle.type_command ("start");
    masq_handle.type_command ("shutdown");
    masq_handle.type_command ("exit");

    let (stdout, stderr) = masq_handle.get_response ();

    assert_eq! (stdout.contains ("neighborhood-mode         zero-hop"), true, "Received '{}'", stdout);
    assert_eq! (stdout.contains ("MASQNode successfully started as process"), true, "Received '{}'", stdout);
    assert_eq! (stdout.contains ("MASQNode was instructed to shut down and has broken its connection"), true, "Received '{}'", stdout);
    assert_eq! (stderr, "".to_string());

    daemon_handle.kill();
}
