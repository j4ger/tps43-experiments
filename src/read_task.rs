use core::convert::Infallible;

use embassy_rp::{
    gpio::{Input, Level, Output, Pull},
    i2c::{self, Blocking, Config},
    peripherals::I2C1,
};
use embassy_time::{Duration, Timer, block_for};
use iqs5xx::IQS5xx;
use log::{info, warn};

use crate::Trackpad;

// ---------------------------------------------------------------------------
// Delay adapter
// ---------------------------------------------------------------------------

/// Wraps `embassy_time::block_for` to satisfy iqs5xx's `DelayMs<u32>` bound
/// (embedded-hal 0.2).  This is blocking, which is acceptable during the
/// short init/reset phase at task start-up.
struct EmbassyDelay;

impl embedded_hal_02::blocking::delay::DelayMs<u32> for EmbassyDelay {
    fn delay_ms(&mut self, ms: u32) {
        block_for(Duration::from_millis(ms as u64));
    }
}

// ---------------------------------------------------------------------------
// I²C compatibility wrapper  (embedded-hal 1.0 → 0.2)
// ---------------------------------------------------------------------------

/// Newtype that adapts an embedded-hal **1.0** `I2c` implementation to the
/// embedded-hal **0.2** `Write` + `WriteRead` blocking traits that iqs5xx
/// requires.
struct I2cCompat<T>(T);

impl<T> embedded_hal_02::blocking::i2c::Write for I2cCompat<T>
where
    T: embedded_hal_1::i2c::I2c,
{
    type Error = T::Error;

    fn write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.write(addr, bytes)
    }
}

impl<T> embedded_hal_02::blocking::i2c::WriteRead for I2cCompat<T>
where
    T: embedded_hal_1::i2c::I2c,
{
    type Error = T::Error;

    fn write_read(&mut self, addr: u8, bytes: &[u8], buffer: &mut [u8]) -> Result<(), Self::Error> {
        self.0.write_read(addr, bytes, buffer)
    }
}

// ---------------------------------------------------------------------------
// GPIO compatibility wrappers  (embedded-hal 1.0 → 0.2)
// ---------------------------------------------------------------------------

/// Adapts an embassy-rp `Input` pin to the embedded-hal 0.2 `InputPin` trait.
struct InputCompat<'d>(Input<'d>);

impl<'d> embedded_hal_02::digital::v2::InputPin for InputCompat<'d> {
    type Error = Infallible;

    fn is_high(&self) -> Result<bool, Self::Error> {
        Ok(self.0.is_high())
    }

    fn is_low(&self) -> Result<bool, Self::Error> {
        Ok(self.0.is_low())
    }
}

/// Adapts an embassy-rp `Output` pin to the embedded-hal 0.2 `OutputPin` trait.
struct OutputCompat<'d>(Output<'d>);

impl<'d> embedded_hal_02::digital::v2::OutputPin for OutputCompat<'d> {
    type Error = Infallible;

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.0.set_high();
        Ok(())
    }

    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.0.set_low();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[embassy_executor::task]
pub async fn read_task(trackpad: Trackpad) {
    // Build a *blocking* I²C peripheral – no interrupt handler required.
    let bus: i2c::I2c<'_, I2C1, Blocking> =
        i2c::I2c::new_blocking(trackpad.i2c, trackpad.scl, trackpad.sda, Config::default());

    // RDY is an open-drain active-high output from the IQS5xx, so floating
    // input is correct.  RST is active-low; drive it high at rest.
    let rdy = Input::new(trackpad.rdy, Pull::None);
    let rst = Output::new(trackpad.rst, Level::High);

    let mut delay = EmbassyDelay;
    let mut device = IQS5xx::new(
        I2cCompat(bus),
        iqs5xx::DEFAULT_I2C_ADDR,
        InputCompat(rdy),
        OutputCompat(rst),
    );

    // -----------------------------------------------------------------------
    // Reset and initialise
    // -----------------------------------------------------------------------

    info!("IQS5xx: task started");
    info!("IQS5xx: configuring blocking I2C + GPIO complete");
    info!("IQS5xx: resetting…");
    if let Err(err) = device.reset(&mut delay) {
        warn!("IQS5xx: reset failed: {:?}", err);
        return;
    }

    info!("IQS5xx: reset complete");
    info!("IQS5xx: waiting for ready high…");
    if let Err(err) = device.poll_ready(&mut delay) {
        warn!("IQS5xx: ready-high wait failed: {:?}", err);
        return;
    }

    info!("IQS5xx: ready high observed, sending init");
    if let Err(err) = device.init() {
        warn!("IQS5xx: init failed: {:?}", err);
        return;
    }
    info!("IQS5xx: initialised");

    // -----------------------------------------------------------------------
    // Read and log device information
    // -----------------------------------------------------------------------

    info!("IQS5xx: reading device info…");
    let info_result = device.transact(&mut delay, |d| {
        let info = d.get_info()?;
        let active_timeout = d.read_reg_u8(0x584)?;
        let idle_touch_timeout = d.read_reg_u8(0x585)?;
        let idle_timeout = d.read_reg_u8(0x586)?;
        let i2c_timeout = d.read_reg_u8(0x58a)?;
        Ok((
            info,
            active_timeout,
            idle_touch_timeout,
            idle_timeout,
            i2c_timeout,
        ))
    });

    match info_result {
        Ok((info, active_to, idle_touch_to, idle_to, i2c_to)) => {
            info!(
                "Device: product={:04x}  project={:04x}  version={}.{:02}  bootloader={:02x}",
                info.product_number,
                info.project_number,
                info.major_ver,
                info.minor_ver,
                info.bootloader_status,
            );
            info!(
                "Timeouts: active={}  idle_touch={}  idle={}  i2c={}",
                active_to, idle_touch_to, idle_to, i2c_to,
            );
        }
        Err(err) => {
            warn!("IQS5xx: failed to read device info: {:?}", err);
            warn!("IQS5xx: continuing into poll loop anyway");
        }
    }

    // -----------------------------------------------------------------------
    // Poll loop – use `try_transact` so we never spin-block the executor.
    // We yield with `Timer::after_millis` between each poll attempt, giving
    // other Embassy tasks (blinker, USB logger, …) a chance to run.
    // -----------------------------------------------------------------------

    info!("IQS5xx: entering poll loop");
    let mut idle_polls: u32 = 0;

    loop {
        match device.try_transact(|d| d.get_report()) {
            Ok(Some(report)) => {
                idle_polls = 0;

                // Raw register dump – always printed so noise-free idle is
                // visible without extra filtering.
                info!(
                    "report  events={:02x}:{:02x}  sysinfo={:02x}:{:02x}  \
                     fingers={}  rel=({}, {})",
                    report.events0,
                    report.events1,
                    report.sys_info0,
                    report.sys_info1,
                    report.num_fingers,
                    report.rel_x,
                    report.rel_y,
                );

                for i in 0..report.num_fingers as usize {
                    let t = &report.touches[i];
                    info!(
                        "  touch[{}]  abs=({}, {})  strength={}  size={}",
                        i as u8, t.abs_x, t.abs_y, t.strength, t.size,
                    );
                }

                // Interpret the raw report as a high-level event and log it.
                // iqs5xx::Event derives core::fmt::Debug, so we bridge through
                // Debug2Format because the crate was compiled against defmt 0.3
                // while this project uses defmt 1.0.
                let event = iqs5xx::Event::from(&report);
                if event != iqs5xx::Event::None {
                    info!("Event: {:?}", event);
                }
            }

            // Device not yet ready – perfectly normal, just wait and retry.
            Ok(None) => {
                idle_polls += 1;
                if idle_polls % 200 == 0 {
                    info!("IQS5xx: still waiting for RDY/report...");
                }
            }

            Err(err) => warn!("IQS5xx: try_transact error: {:?}", err),
        }

        // Yield to the executor; adjust the interval to taste.
        Timer::after_millis(5).await;
    }
}
