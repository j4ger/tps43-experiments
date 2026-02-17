use embassy_time::{Duration, Timer};

#[embassy_executor::task]
pub async fn logger_task() {
    esp_println::println!("Heartbeat");
    Timer::after(Duration::from_millis(1000)).await;
}
