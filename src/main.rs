#![no_std]
#![no_main]

mod logger;
use logger::*;

mod read_task;
use read_task::*;

use embassy_executor::Spawner;
use esp_backtrace as _;
use esp_hal::{interrupt::software::SoftwareInterruptControl, timer::timg::TimerGroup};

// assign_resources! {
//     trackpad: Trackpad {
//         rdy: GPIO2,
//         rst: GPIO3,
//         sda: GPIO4,
//         scl: GPIO5,
//     }
// }

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    let p = esp_hal::init(Default::default());
    let sw_int = SoftwareInterruptControl::new(p.SW_INTERRUPT);
    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    spawner.spawn(logger_task()).unwrap();
}
