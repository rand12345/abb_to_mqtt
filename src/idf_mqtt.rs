use std::sync::{Arc, Mutex};

use embedded_svc::mqtt::client::utils::ConnState;
use embedded_svc::mqtt::client::{Client, Connection, MessageImpl, Publish, QoS};
use esp_idf_svc::mqtt::client::*;
use log::*;

pub(crate) type MqttClientType = EspMqttClient<ConnState<MessageImpl, esp_idf_sys::EspError>>;

pub fn mqtt_client(
    url: String,
    subscription: Vec<String>,
    client_id: Option<&str>,
    topic: String,
    conf: MqttClientConfiguration,
) -> anyhow::Result<MqttClientType> {
    info!("About to start MQTT client");

    let (mut client, mut connection) = EspMqttClient::new_with_conn(url, &conf)?;

    info!("MQTT client started");

    std::thread::spawn(move || {
        info!("MQTT Listening for messages");

        while let Some(msg) = connection.next() {
            match msg {
                Err(e) => info!("MQTT Message ERROR: {}", e),
                Ok(msg) => info!("MQTT Message: {:?}", msg), // handle incomming messages
            }
        }

        info!("MQTT connection loop exit");
    });
    for sub in subscription {
        client.subscribe(&sub, QoS::AtMostOnce)?;
        info!("Subscribed to all topics {}", topic);
    }

    client.publish(
        client_id.unwrap(),
        QoS::AtMostOnce,
        false,
        "Alive".as_bytes(),
    )?;

    info!("Published an alive message to topic {}", topic);

    Ok(client)
}

pub fn mqtt_publish(
    client_m: Arc<Mutex<MqttClientType>>,
    topic: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    if let Ok(mut client) = client_m.lock() {
        client.publish(topic, QoS::AtMostOnce, false, payload)?;
        log::info!(
            "Published {} {:?} {:?} {}",
            topic,
            QoS::AtMostOnce,
            false,
            String::from_utf8_lossy(payload)
        )
    } else {
        info!("MQTT Mutex lock fail")
    }
    Ok(())
}
