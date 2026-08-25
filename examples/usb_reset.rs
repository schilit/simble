//! Resets USB dongles, silencing whatever a dead process left running.
//!
//! A controller is not state that dies with the process driving it: a dongle
//! left advertising keeps advertising — under whatever name was last written
//! into it — until something sends HCI Reset or the stick is re-plugged. The
//! symptom that motivated this tool was a phone still seeing an advertising
//! name that had been renamed in the tree days earlier, beaconed by an idle
//! dongle no process had touched since.
//!
//! With no argument every dongle is reset; with selectors (`#0`, `0a12:0001`,
//! `02/4`, `02.3.4`) only those are.

use simble::transport::HciChannel;
use simble::transport::usb::{UsbSelector, UsbTransport, list_bluetooth_dongles};

fn reset(selector: &UsbSelector, label: &str) {
    let mut transport = match UsbTransport::open_selected(selector) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{label}: could not open ({e})");
            return;
        }
    };
    let channel = HciChannel::new();
    if let Err(e) = channel.send_command(&[0x03, 0x0C, 0x00]) {
        eprintln!("{label}: could not queue Reset ({e})");
        return;
    }
    // Command Complete for Reset: event 0x0E, opcode 0x0C03.
    for _ in 0..2000 {
        if transport.pump(&channel).is_err() {
            break;
        }
        while let Some(p) = channel.poll_controller_packet() {
            if p.len() >= 7 && p[1] == 0x0E && p[4] == 0x03 && p[5] == 0x0C {
                println!("{label}: reset (status {:#04x})", p[6]);
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    eprintln!("{label}: no Command Complete for Reset");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        let dongles = match list_bluetooth_dongles() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("could not list dongles: {e}");
                std::process::exit(1);
            }
        };
        if dongles.is_empty() {
            println!("no Bluetooth-class USB dongles found");
            return;
        }
        for dongle in &dongles {
            let spec = dongle.port_selector();
            match UsbSelector::parse(&spec) {
                Ok(selector) => reset(&selector, &dongle.describe()),
                Err(e) => eprintln!("{}: bad selector {spec} ({e})", dongle.describe()),
            }
        }
    } else {
        for spec in &args {
            match UsbSelector::parse(spec) {
                Ok(selector) => reset(&selector, spec),
                Err(e) => eprintln!("{spec}: {e}"),
            }
        }
    }
}
