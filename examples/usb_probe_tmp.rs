//! Temporary probe: what does this controller admit to supporting?
use simble::transport::usb::{UsbSelector, UsbTransport};
use simble::transport::HciChannel;

fn ask(t: &mut UsbTransport, c: &HciChannel, cmd: &[u8], name: &str) {
    c.send_command(cmd).unwrap();
    for _ in 0..2000 {
        t.pump(c).unwrap();
        while let Some(p) = c.poll_controller_packet() {
            if p.len() >= 7 && p[1] == 0x0E {
                println!("{name}: opcode {:02X}{:02X} status {:#04x} params {:02X?}",
                    p[5], p[4], p[6], &p[7..p.len().min(20)]);
                return;
            }
            if p.len() >= 6 && p[1] == 0x0F {
                println!("{name}: Command Status {:#04x}", p[3]);
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    println!("{name}: no answer");
}

fn main() {
    let sel = UsbSelector::parse("02.3.3").unwrap();
    let mut t = UsbTransport::open_selected(&sel).unwrap();
    let c = HciChannel::new();
    ask(&mut t, &c, &[0x03, 0x0C, 0x00], "Reset");
    // Read Local Supported Commands
    c.send_command(&[0x02, 0x10, 0x00]).unwrap();
    for _ in 0..2000 {
        t.pump(&c).unwrap();
        let mut done = false;
        while let Some(p) = c.poll_controller_packet() {
            if p.len() >= 7 && p[1] == 0x0E && p[4] == 0x02 && p[5] == 0x10 {
                let bits = &p[7..];
                // Octets of interest (Core v5.4 Vol 4 Part E §6.27):
                // LE Set Periodic Advertising Data = octet 37 bit 6? print raw
                println!("supported-commands octets 35..46: {:02X?}", &bits[35..46.min(bits.len())]);
                done = true;
            }
        }
        if done { break; }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // LE Create BIG with plausible params (will fail without a periodic train,
    // but HOW it fails is the answer: Unknown Command 0x01 = not in the build)
    let create_big = [
        0x68, 0x20, 0x1F, 0x00, 0x00, 0x01, 0x10, 0x27, 0x00, 0x64, 0x00, 0x28, 0x00,
        0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    ask(&mut t, &c, &create_big, "LE_Create_BIG");
    // Recreate the failing sequence, then vary the data length.
    // Ext adv params (set 0, non-conn non-scan, from the failing trace shape)
    ask(&mut t, &c, &[0x36, 0x20, 0x19, 0x00, 0x00, 0x00, 0xA0, 0x00, 0x00, 0xA0, 0x00, 0x00,
        0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7F, 0x01, 0x00, 0x02, 0x00, 0x00],
        "ext_adv_params");
    ask(&mut t, &c, &[0x3E, 0x20, 0x07, 0x00, 0x50, 0x00, 0x50, 0x00, 0x00, 0x00], "periodic_params");
    // small periodic data: 6 bytes
    ask(&mut t, &c, &[0x3F, 0x20, 0x09, 0x00, 0x03, 0x06, 0x05, 0x16, 0x52, 0x18, 0x01, 0x02],
        "periodic_data_6B");
    for n in [25usize, 31, 32, 46] {
        let mut cmd = vec![0x3F, 0x20, (3 + n) as u8, 0x00, 0x03, n as u8];
        cmd.extend(std::iter::repeat(0x11).take(n));
        ask(&mut t, &c, &cmd, &format!("periodic_data_{n}B"));
    }
}
