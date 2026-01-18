use embassy_rp::gpio::{Level, Output};
use embassy_time::Timer;

use crate::Led;

#[embassy_executor::task]
pub async fn blinker_task(led: Led) {
    let mut led = Output::new(led.led, Level::Low);

    loop {
        log::info!("led on!");
        led.set_high();
        Timer::after_secs(1).await;

        log::info!("led off!");
        led.set_low();
        Timer::after_secs(1).await;
    }
}
