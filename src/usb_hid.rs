use core::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::join::join;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_usb::class::hid::{HidReaderWriter, ReportId, RequestHandler, State};
use embassy_usb::control::OutResponse;
use embassy_usb::{Builder, Config, Handler};
use log::{info, warn};
use usbd_hid::descriptor::{MouseReport, SerializedDescriptor};

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

pub static MOUSE_REPORT_CHANNEL: Channel<CriticalSectionRawMutex, MouseReport, 8> = Channel::new();

#[embassy_executor::task]
pub async fn usb_hid_task(usb: embassy_rp::Peri<'static, USB>) {
    info!("USB HID: task started");

    let driver = Driver::new(usb, Irqs);

    let mut config = Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("tps43");
    config.product = Some("Trackpad Mouse");
    config.serial_number = Some("tps43-trackpad");
    config.max_power = 100;
    config.max_packet_size_0 = 64;
    config.composite_with_iads = false;
    config.device_class = 0;
    config.device_sub_class = 0;
    config.device_protocol = 0;

    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut msos_descriptor = [0; 256];
    let mut control_buf = [0; 64];
    let mut request_handler = MouseRequestHandler {};
    let mut device_handler = MouseDeviceHandler::new();
    let mut state = State::new();

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut msos_descriptor,
        &mut control_buf,
    );

    builder.handler(&mut device_handler);

    let config = embassy_usb::class::hid::Config {
        report_descriptor: MouseReport::desc(),
        request_handler: None,
        poll_ms: 10,
        max_packet_size: 8,
    };
    let hid = HidReaderWriter::<_, 1, 8>::new(&mut builder, &mut state, config);

    let mut usb = builder.build();
    let usb_fut = usb.run();

    let (reader, mut writer) = hid.split();

    let in_fut = async {
        loop {
            let report = MOUSE_REPORT_CHANNEL.receive().await;
            match writer.write_serialize(&report).await {
                Ok(()) => {}
                Err(e) => warn!("USB HID: failed to send report: {:?}", e),
            }
        }
    };

    let out_fut = async {
        reader.run(false, &mut request_handler).await;
    };

    join(usb_fut, join(in_fut, out_fut)).await;
}

struct MouseRequestHandler {}

impl RequestHandler for MouseRequestHandler {
    fn get_report(&mut self, id: ReportId, _buf: &mut [u8]) -> Option<usize> {
        info!("USB HID: get_report for {:?}", id);
        None
    }

    fn set_report(&mut self, id: ReportId, data: &[u8]) -> OutResponse {
        info!("USB HID: set_report for {:?}: {:?}", id, data);
        OutResponse::Accepted
    }

    fn set_idle_ms(&mut self, id: Option<ReportId>, dur: u32) {
        info!("USB HID: set idle for {:?} to {} ms", id, dur);
    }

    fn get_idle_ms(&mut self, id: Option<ReportId>) -> Option<u32> {
        info!("USB HID: get idle for {:?}", id);
        None
    }
}

struct MouseDeviceHandler {
    configured: AtomicBool,
}

impl MouseDeviceHandler {
    fn new() -> Self {
        Self {
            configured: AtomicBool::new(false),
        }
    }
}

impl Handler for MouseDeviceHandler {
    fn enabled(&mut self, enabled: bool) {
        self.configured.store(false, Ordering::Relaxed);
        if enabled {
            info!("USB HID: device enabled");
        } else {
            info!("USB HID: device disabled");
        }
    }

    fn reset(&mut self) {
        self.configured.store(false, Ordering::Relaxed);
        info!("USB HID: bus reset");
    }

    fn addressed(&mut self, addr: u8) {
        self.configured.store(false, Ordering::Relaxed);
        info!("USB HID: address set to {}", addr);
    }

    fn configured(&mut self, configured: bool) {
        self.configured.store(configured, Ordering::Relaxed);
        if configured {
            info!("USB HID: device configured");
        } else {
            info!("USB HID: device no longer configured");
        }
    }
}
