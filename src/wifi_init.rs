use anyhow::Result;
use embedded_svc::ipv4::{self};
use embedded_svc::ping::Ping;
use embedded_svc::wifi::*;
use esp_idf_svc::netif::EspNetifStack;
use esp_idf_svc::nvs::EspDefaultNvs;
use esp_idf_svc::ping;
use esp_idf_svc::sysloop::EspSysLoopStack;
use esp_idf_svc::wifi::EspWifi;
use log::info;
use std::sync::Arc;
use std::time::Duration;

pub fn wifi(
    netif_stack: Arc<EspNetifStack>,
    sys_loop_stack: Arc<EspSysLoopStack>,
    default_nvs: Arc<EspDefaultNvs>,
    ssid: &str,
    pass: &str,
) -> Result<Box<EspWifi>> {
    let mut wifi = Box::new(EspWifi::new(netif_stack, sys_loop_stack, default_nvs)?);

    info!("Wifi created, about to scan");

    let ap_infos = wifi.scan()?;

    let ours = ap_infos.into_iter().find(|a| a.ssid == ssid);

    let channel = if let Some(ours) = ours {
        info!(
            "Found configured access point {} on channel {}",
            ssid, ours.channel
        );
        Some(ours.channel)
    } else {
        info!(
            "Configured access point {} not found during scanning, will go with unknown channel",
            ssid
        );
        None
    };
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid.into(),
        password: pass.into(),
        channel,
        ..Default::default()
    }))?;
    // wifi.set_configuration(&Configuration::Mixed(
    //     ClientConfiguration {
    //         ssid: ssid.into(),
    //         password: pass.into(),
    //         channel,
    //         ..Default::default()
    //     },
    //     AccessPointConfiguration {
    //         ssid: "aptest".into(),
    //         channel: channel.unwrap_or(1),
    //         ..Default::default()
    //     },
    // ))?;

    info!("Wifi configuration set, about to get status");

    if wifi
        .wait_status_with_timeout(Duration::from_secs(60), |status| !status.is_transitional())
        .map_err(|e| info!("Unexpected Wifi status: {:?}", e))
        .is_err()
    {
        println!("Debug: wifi error");
    };
    let status = wifi.get_status();

    // if let Status(
    //     ClientStatus::Started(ClientConnectionStatus::Connected(ClientIpStatus::Done(ip_settings))),
    //     ApStatus::Started(ApIpStatus::Done),
    // ) = status
    if let Status(
        ClientStatus::Started(ClientConnectionStatus::Connected(ClientIpStatus::Done(ip_settings))),
        _,
    ) = status
    {
        info!("Wifi connected");

        ping_init(&ip_settings)?;
    } else {
        panic!("Ping gateway failed: {:?}", status);
    }
    Ok(wifi)
}

fn ping_init(ip_settings: &ipv4::ClientSettings) -> Result<()> {
    info!("About to do some pings for {:?}", ip_settings);

    let ping_summary =
        ping::EspPing::default().ping(ip_settings.subnet.gateway, &Default::default())?;
    if ping_summary.transmitted != ping_summary.received {
        panic!(
            "Pinging gateway {} resulted in timeouts",
            ip_settings.subnet.gateway
        );
    }
    info!("Pinging done");
    Ok(())
}

#[allow(dead_code)]
pub fn check_state(wifi: &EspWifi) -> Result<()> {
    if wifi
        .wait_status_with_timeout(Duration::from_secs(1), |status| !status.is_transitional())
        .map_err(|e| info!("Unexpected Wifi status: {:?}", e))
        .is_err()
    {
        println!("Debug: wifi error");
    };
    let status = wifi.get_status();
    if let Status(
        ClientStatus::Started(ClientConnectionStatus::Connected(ClientIpStatus::Done(
            _ip_settings,
        ))),
        _,
    ) = status
    {
        return Ok(());
    }
    panic!("Wifi offline");
}
