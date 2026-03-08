use drv2605_async::{Drv2605, Effect};
use embassy_rp::{
    bind_interrupts,
    i2c::{self, Config, InterruptHandler},
    peripherals::I2C0,
};
use embassy_time::Timer;
use log::{info, warn};

use crate::Vibrator;

bind_interrupts!(
    struct Irqs {
        I2C0_IRQ => InterruptHandler<I2C0>;
    }
);

#[embassy_executor::task]
pub async fn vibrator_task(vibrator: Vibrator) {
    info!("DRV2605: task started");

    let bus = i2c::I2c::new_async(
        vibrator.i2c,
        vibrator.scl,
        vibrator.sda,
        Irqs,
        Config::default(),
    );

    let mut drv = Drv2605::new(bus);

    info!("DRV2605: resetting");
    if let Err(err) = drv.reset().await {
        warn!("DRV2605: reset failed: {:?}", err);
        return;
    }

    Timer::after_millis(10).await;

    info!("DRV2605: initializing open-loop ERM mode");
    if let Err(err) = drv.init_open_loop_erm().await {
        warn!("DRV2605: init failed: {:?}", err);
        return;
    }

    info!("DRV2605: selecting sharp click effect");
    if let Err(err) = drv.set_single_effect(Effect::SharpClick100).await {
        warn!("DRV2605: set_single_effect failed: {:?}", err);
        return;
    }

    info!("DRV2605: entering periodic click loop");
    loop {
        info!("DRV2605: click");
        if let Err(err) = drv.set_go(true).await {
            warn!("DRV2605: set_go(true) failed: {:?}", err);
        }

        Timer::after_secs(1).await;
    }
}
