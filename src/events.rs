use esp_idf_hal::mutex::Mutex;
use esp_idf_svc::timer::*;

use crate::aurora::{Aurora, AuroraInverter};
use crate::idf_mqtt::{mqtt_publish, MqttClientType};
use crate::MQTT_TOPIC_NAME;
// use log::info;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn inverter_poll_task(
    inverters_arc_mutex: Arc<Mutex<Vec<AuroraInverter>>>,
    aurora_arc_mutex: Arc<Mutex<Aurora>>,
    mqttclient_arc_mutex: Arc<Mutex<MqttClientType>>,
    boot_time: Instant,
) {
    let mut aurora = aurora_arc_mutex.lock();
    let mut inverters = inverters_arc_mutex.lock();
    for inverter in inverters.iter_mut() {
        let json_data = {
            if aurora.poll_inverter(inverter).is_err() {
                println!("Poll error on ABB{}", inverter.id())
            };
            // send zeroed data if error - clears MQTT
            aurora.data_to_vec_mqtt_json(inverter, MQTT_TOPIC_NAME)
        };
        if let Ok(d) = json_data {
            d.iter().for_each(|m| {
                if let Err(e) =
                    mqtt_publish(mqttclient_arc_mutex.clone(), &m.topic, m.payload.as_bytes())
                {
                    println!("mqtt_publish error {:?} {:#?}", e, d);
                };
            });

            // update alive time update
            let message = format!("Uptime {:?}", Instant::now().duration_since(boot_time));
            if let Err(e) = mqtt_publish(
                mqttclient_arc_mutex.clone(),
                MQTT_TOPIC_NAME,
                message.as_bytes(),
            ) {
                println!("mqtt_publish error {:?} {:#?}", e, d);
            };
        }
    }
}

pub fn periodic_inverter_event(
    inverters: Arc<Mutex<Vec<AuroraInverter>>>,
    aurora: Arc<Mutex<Aurora>>,
    mqttclient: Arc<Mutex<MqttClientType>>,
    poll_frequency: Duration,
    boot_time: Instant,
) -> anyhow::Result<EspTimer> {
    use embedded_svc::timer::PeriodicTimer;
    use embedded_svc::timer::TimerService as _;
    let mut periodic_timer = esp_idf_svc::timer::EspTimerService::new()?.timer(move || {
        inverter_poll_task(
            inverters.clone(),
            aurora.clone(),
            mqttclient.clone(),
            boot_time,
        );
    })?;

    periodic_timer.every(poll_frequency)?;

    Ok(periodic_timer)
}
