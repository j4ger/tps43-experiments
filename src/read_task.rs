use core::{
    convert::Infallible,
    sync::atomic::{AtomicBool, Ordering},
};

use embassy_rp::{
    gpio::{Input, Level, Output, Pull},
    i2c::{self, Blocking, Config},
    peripherals::I2C1,
};
use embassy_time::Timer;
use iqs5xx::IQS5xx;
use log::{info, warn};
use usbd_hid::descriptor::MouseReport;

use crate::{Trackpad, usb_hid::MOUSE_REPORT_CHANNEL};

static RDY_LEVEL: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// I²C compatibility wrapper (embedded-hal 1.0 -> 0.2)
// ---------------------------------------------------------------------------

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
// RDY ready-level proxy
// ---------------------------------------------------------------------------

struct RdyProxy;

impl embedded_hal_02::digital::v2::InputPin for RdyProxy {
    type Error = Infallible;

    fn is_high(&self) -> Result<bool, Self::Error> {
        Ok(RDY_LEVEL.load(Ordering::Acquire))
    }

    fn is_low(&self) -> Result<bool, Self::Error> {
        Ok(!RDY_LEVEL.load(Ordering::Acquire))
    }
}

// ---------------------------------------------------------------------------
// GPIO compatibility wrapper for reset pin
// ---------------------------------------------------------------------------

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
// Helpers
// ---------------------------------------------------------------------------

async fn wait_rdy_high(rdy: &mut Input<'_>) {
    if !rdy.is_high() {
        rdy.wait_for_high().await;
    }
    RDY_LEVEL.store(true, Ordering::Release);
}

async fn wait_rdy_low(rdy: &mut Input<'_>) {
    if !rdy.is_low() {
        rdy.wait_for_low().await;
    }
    RDY_LEVEL.store(false, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[embassy_executor::task]
pub async fn read_task(trackpad: Trackpad) {
    let bus: i2c::I2c<'_, I2C1, Blocking> =
        i2c::I2c::new_blocking(trackpad.i2c, trackpad.scl, trackpad.sda, Config::default());

    let mut rdy = Input::new(trackpad.rdy, Pull::None);
    let mut rst = Output::new(trackpad.rst, Level::High);

    info!("IQS5xx: task started");
    info!("IQS5xx: configuring blocking I2C + interrupt-driven RDY complete");

    // Manual reset before handing RST ownership to the driver.
    info!("IQS5xx: resetting…");
    rst.set_low();
    Timer::after_millis(10).await;
    rst.set_high();
    Timer::after_millis(10).await;
    info!("IQS5xx: reset complete");

    info!("IQS5xx: waiting for first RDY high…");
    wait_rdy_high(&mut rdy).await;
    info!("IQS5xx: first RDY high observed");

    let mut device = IQS5xx::new(
        I2cCompat(bus),
        iqs5xx::DEFAULT_I2C_ADDR,
        RdyProxy,
        OutputCompat(rst),
    );

    info!("IQS5xx: sending init");
    if let Err(err) = device.init() {
        warn!("IQS5xx: init failed: {:?}", err);
        return;
    }

    wait_rdy_low(&mut rdy).await;
    info!("IQS5xx: initialised");

    // -----------------------------------------------------------------------
    // Read and log device information using one interrupt-driven transaction.
    // -----------------------------------------------------------------------

    info!("IQS5xx: waiting for RDY to read device info…");
    wait_rdy_high(&mut rdy).await;

    let info_result = (|| {
        let info = device.get_info()?;
        let active_timeout = device.read_reg_u8(0x584)?;
        let idle_touch_timeout = device.read_reg_u8(0x585)?;
        let idle_timeout = device.read_reg_u8(0x586)?;
        let i2c_timeout = device.read_reg_u8(0x58a)?;
        device.end_session()?;
        Ok::<_, iqs5xx::Error>((
            info,
            active_timeout,
            idle_touch_timeout,
            idle_timeout,
            i2c_timeout,
        ))
    })();

    wait_rdy_low(&mut rdy).await;

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
        }
    }

    // -----------------------------------------------------------------------
    // Interrupt-driven report loop.
    // -----------------------------------------------------------------------

    info!("IQS5xx: entering interrupt-driven poll loop");

    loop {
        wait_rdy_high(&mut rdy).await;

        match device.try_transact(|d| d.get_report()) {
            Ok(Some(report)) => {
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

                let x = report.rel_x.clamp(i8::MIN as i16, i8::MAX as i16) as i8;
                let y = report.rel_y.clamp(i8::MIN as i16, i8::MAX as i16) as i8;

                if x != 0 || y != 0 {
                    MOUSE_REPORT_CHANNEL
                        .send(MouseReport {
                            buttons: 0,
                            x,
                            y,
                            wheel: 0,
                            pan: 0,
                        })
                        .await;
                }

                let event = iqs5xx::Event::from(&report);
                if event != iqs5xx::Event::None {
                    info!("Event: {:?}", event);
                }
            }
            Ok(None) => {
                warn!("IQS5xx: RDY high fired but no report was available");
            }
            Err(err) => {
                warn!("IQS5xx: try_transact error: {:?}", err);
            }
        }

        wait_rdy_low(&mut rdy).await;
    }
}
