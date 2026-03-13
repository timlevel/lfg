use std::time::Duration;
use tokio::sync::RwLock;
use std::sync::Arc;
use tracing::{info, warn};

use crate::event;
use crate::render;
use crate::state::DisplayState;

// BLE UUIDs
const WRITE_UUIDS: &[&str] = &[
    "0000fa02-0000-1000-8000-00805f9b34fb",
    "0000fff2-0000-1000-8000-00805f9b34fb",
];
#[allow(dead_code)]
const NOTIFY_UUIDS: &[&str] = &[
    "0000fa03-0000-1000-8000-00805f9b34fb",
    "0000fff1-0000-1000-8000-00805f9b34fb",
];

const CMD_SCREEN_ON: &[u8] = &[0x05, 0x00, 0x07, 0x01, 0x01];
const CMD_BRIGHTNESS: &[u8] = &[0x05, 0x00, 0x04, 0x80, 100];

const BLE_DEBOUNCE_SECS: f64 = 2.0;
const BLE_POLL_SECS: f64 = 0.25;
const BLE_HEARTBEAT_SECS: f64 = 30.0;

/// Build GIF packets for BLE transmission.
/// Each packet has a 16-byte header + up to 4096 bytes of GIF data.
pub fn build_gif_packets(gif_data: &[u8]) -> Vec<Vec<u8>> {
    const CHUNK_SIZE: usize = 4096;
    let crc = crc32fast::hash(gif_data);
    let mut packets = Vec::new();

    for (i, chunk) in gif_data.chunks(CHUNK_SIZE).enumerate() {
        let mut hdr = vec![0u8; 16];
        let total_len = (chunk.len() + 16) as u16;
        hdr[0] = (total_len & 0xFF) as u8;
        hdr[1] = ((total_len >> 8) & 0xFF) as u8;
        hdr[2] = 0x01;
        hdr[3] = 0x00;
        hdr[4] = if i > 0 { 0x02 } else { 0x00 };

        let gif_len = gif_data.len() as u32;
        hdr[5] = (gif_len & 0xFF) as u8;
        hdr[6] = ((gif_len >> 8) & 0xFF) as u8;
        hdr[7] = ((gif_len >> 16) & 0xFF) as u8;
        hdr[8] = ((gif_len >> 24) & 0xFF) as u8;

        hdr[9] = (crc & 0xFF) as u8;
        hdr[10] = ((crc >> 8) & 0xFF) as u8;
        hdr[11] = ((crc >> 16) & 0xFF) as u8;
        hdr[12] = ((crc >> 24) & 0xFF) as u8;

        hdr[13] = 0x05;
        hdr[14] = 0x00;
        hdr[15] = 0x0D;

        let mut packet = hdr;
        packet.extend_from_slice(chunk);
        packets.push(packet);
    }

    packets
}

/// Main BLE render loop. Runs in a spawned task.
pub async fn ble_loop(state: Arc<RwLock<DisplayState>>) {
    use btleplug::api::{Peripheral as _, WriteType};

    loop {
        // Try to connect
        let connection = match connect_ble().await {
            Some(c) => c,
            None => {
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        };

        let (peripheral, write_char) = connection;
        let mut last_hash: Option<String> = None;
        let mut change_detected_at: Option<tokio::time::Instant> = None;
        let mut last_successful_write = tokio::time::Instant::now();

        info!("BLE render loop started");

        loop {
            // Check connectivity
            match peripheral.is_connected().await {
                Ok(false) => {
                    warn!("BLE device disconnected — reconnecting");
                    break;
                }
                Err(e) => {
                    warn!("BLE connectivity check failed: {} — reconnecting", e);
                    break;
                }
                Ok(true) => {}
            }

            // Check stale under write lock, then snapshot under read
            {
                let mut s = state.write().await;
                event::check_stale(&mut s);
            }
            let (current_hash, snap, any_requesting) = {
                let s = state.read().await;
                let hash = render::state_hash(&s);
                let snap = render::snapshot_state(&s);
                let any_req = s.any_requesting();
                (hash, snap, any_req)
            };

            let now = tokio::time::Instant::now();

            if Some(&current_hash) != last_hash.as_ref() {
                if change_detected_at.is_none() {
                    change_detected_at = Some(now);
                    info!("State change detected, debouncing {:.1}s", BLE_DEBOUNCE_SECS);
                }

                let elapsed = now.duration_since(change_detected_at.unwrap());
                if elapsed.as_secs_f64() >= BLE_DEBOUNCE_SECS {
                    let gif_data = render::build_animated_gif(&snap, any_requesting);
                    let packets = build_gif_packets(&gif_data);

                    let mut send_ok = true;
                    for pkt in &packets {
                        match peripheral.write(
                            &write_char,
                            pkt,
                            WriteType::WithResponse,
                        ).await {
                            Ok(_) => {
                                if packets.len() > 1 {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                            }
                            Err(e) => {
                                warn!("BLE write error: {} — reconnecting", e);
                                send_ok = false;
                                break;
                            }
                        }
                    }

                    if !send_ok {
                        break; // reconnect
                    }

                    last_hash = Some(current_hash);
                    change_detected_at = None;
                    last_successful_write = now;
                    info!(
                        "Sent animated GIF ({} bytes, {})",
                        gif_data.len(),
                        if any_requesting { "fast" } else { "normal" }
                    );
                }
            } else {
                change_detected_at = None;

                // Heartbeat: send brightness command periodically to detect stale connections
                if now.duration_since(last_successful_write).as_secs_f64() >= BLE_HEARTBEAT_SECS {
                    match peripheral.write(&write_char, CMD_BRIGHTNESS, WriteType::WithResponse).await {
                        Ok(_) => {
                            last_successful_write = now;
                        }
                        Err(e) => {
                            warn!("BLE heartbeat failed: {} — reconnecting", e);
                            break;
                        }
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs_f64(BLE_POLL_SECS)).await;
        }

        // Cleanup on disconnect
        let _ = peripheral.disconnect().await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn connect_ble() -> Option<(btleplug::platform::Peripheral, btleplug::api::Characteristic)> {
    use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
    use btleplug::platform::Manager;

    let manager = Manager::new().await.ok()?;
    let adapters = manager.adapters().await.ok()?;
    let adapter = adapters.into_iter().next()?;

    info!("Scanning for IDM-* devices...");
    adapter.start_scan(ScanFilter::default()).await.ok()?;
    tokio::time::sleep(Duration::from_secs(6)).await;
    adapter.stop_scan().await.ok();

    let peripherals = adapter.peripherals().await.ok()?;

    // Find IDM-* device
    let mut idm_peripheral = None;
    let mut idm_name = String::from("unknown");
    for p in peripherals {
        if let Ok(Some(props)) = p.properties().await {
            if let Some(name) = &props.local_name {
                if name.starts_with("IDM-") {
                    idm_name = name.clone();
                    idm_peripheral = Some(p);
                    break;
                }
            }
        }
    }
    let idm = idm_peripheral?;
    info!("Found {} at {:?}", idm_name, idm.id());

    idm.connect().await.ok()?;
    idm.discover_services().await.ok()?;

    // Find write characteristic (resolve once, cache the handle)
    let mut write_char = None;
    for svc in idm.services() {
        for ch in &svc.characteristics {
            let uuid_str = ch.uuid.to_string();
            if WRITE_UUIDS.iter().any(|&u| u == uuid_str) {
                info!("Using write UUID: {}", uuid_str);
                write_char = Some(ch.clone());
                break;
            }
        }
        if write_char.is_some() {
            break;
        }
    }
    let write_char = write_char?;

    // Send screen on + brightness
    let _ = idm.write(&write_char, CMD_SCREEN_ON, WriteType::WithoutResponse).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = idm.write(&write_char, CMD_BRIGHTNESS, WriteType::WithoutResponse).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    info!("Connected to iDotMatrix — screen ON");
    Some((idm, write_char))
}
