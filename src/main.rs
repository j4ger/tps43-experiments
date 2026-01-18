#![no_std]
#![no_main]

mod logger;
use logger::*;

mod read_task;
use read_task::*;

mod blinker;
use blinker::*;

use assign_resources::assign_resources;
use embassy_executor::Spawner;
use embassy_rp::{Peri, peripherals};
use {defmt_rtt as _, panic_probe as _};

assign_resources! {
    led: Led {
        led: PIN_25,
    },
    trackpad: Trackpad {
        rdy: PIN_9,
        rst: PIN_8,
        // I2C1
        scl: PIN_7,
        sda: PIN_6,
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let r = split_resources!(p);
    let usb = p.USB;

    spawner.spawn(logger_task(usb)).unwrap();
    spawner.spawn(blinker_task(r.led)).unwrap();
}
