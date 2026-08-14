//! SMART / storage health summary for the selected physical disk (Windows WMI).

#[derive(Debug, Clone)]
pub enum SmartHealth {
    Healthy,
    Failing { reasons: Vec<String> },
    Disabled,
}

#[cfg(windows)]
mod windows {
    use serde::Deserialize;
    use wmi::WMIConnection;

    use super::SmartHealth;

    #[derive(Debug, Deserialize)]
    #[serde(rename = "MSFT_PhysicalDisk")]
    #[allow(non_snake_case)]
    struct MsftPhysicalDiskRow {
        HealthStatus: Option<u16>,
        OperationalStatus: Option<wmi::Variant>,
        OperationalDetails: Option<wmi::Variant>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename = "MSStorageDriver_FailurePredictStatus")]
    #[allow(non_snake_case)]
    struct FailurePredictRow {
        InstanceName: Option<String>,
        PredictFailure: Option<bool>,
        Reason: Option<u32>,
    }

    fn variant_to_u16_vec(v: &wmi::Variant) -> Vec<u16> {
        match v {
            wmi::Variant::Array(items) => items
                .iter()
                .filter_map(|x| match x {
                    wmi::Variant::UI2(n) => Some(*n),
                    wmi::Variant::UI4(n) => Some(*n as u16),
                    wmi::Variant::I4(n) => Some(*n as u16),
                    _ => None,
                })
                .collect(),
            wmi::Variant::UI2(n) => vec![*n],
            _ => Vec::new(),
        }
    }

    fn variant_to_string_vec(v: &wmi::Variant) -> Vec<String> {
        match v {
            wmi::Variant::Array(items) => items
                .iter()
                .filter_map(|x| match x {
                    wmi::Variant::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            wmi::Variant::String(s) => vec![s.clone()],
            _ => Vec::new(),
        }
    }

    /// MI OperationalStatus — include non-OK states as extra context (MSFT_StorageFaultDomain style).
    fn operational_status_label(code: u16) -> Option<&'static str> {
        match code {
            2 => None, // OK
            3 => Some("Degraded"),
            4 => Some("Stressed"),
            5 => Some("Predictive failure"),
            6 => Some("Error"),
            7 => Some("Non-recoverable error"),
            11 => Some("In service"),
            12 => Some("No contact"),
            13 => Some("Lost communication"),
            15 => Some("Dormant"),
            _ => None,
        }
    }

    /// Returns `None` when no `MSStorageDriver_FailurePredictStatus` entry exists for this disk
    /// (i.e. the drive does not expose SMART data — typical for USB flash drives, SD cards, etc.).
    /// Returns `Some((predict_failure, reason))` when a SMART entry is present.
    ///
    /// Matching uses the PNP device ID from `Win32_DiskDrive` (case-insensitive).
    /// The InstanceName in `MSStorageDriver_FailurePredictStatus` is the PNP device ID
    /// with a `_0` suffix appended, e.g.:
    ///   `SCSI\Disk&Ven_ATA&Prod_Samsung_SSD_850\4&ed1fbf&0&020400_0`
    fn failure_predict_for_disk(pnp_device_id: &str) -> Option<(bool, String)> {
        if pnp_device_id.is_empty() {
            log::warn!(target: "diskoria", "SMART: pnp_device_id is empty, skipping FailurePredict lookup");
            return None;
        }
        log::info!(target: "diskoria", "SMART: looking up FailurePredict for pnp_device_id={pnp_device_id:?}");
        let wmi = match WMIConnection::with_namespace_path(r"ROOT\WMI") {
            Ok(w) => w,
            Err(e) => {
                log::warn!(target: "diskoria", "SMART: failed to connect to ROOT\\WMI: {e}");
                return None;
            }
        };
        let rows: Vec<FailurePredictRow> = match wmi.raw_query(
            "SELECT InstanceName, PredictFailure, Reason FROM MSStorageDriver_FailurePredictStatus",
        ) {
            Ok(r) => r,
            Err(e) => {
                log::warn!(target: "diskoria", "SMART: FailurePredictStatus query failed: {e}");
                return None;
            }
        };
        log::info!(target: "diskoria", "SMART: got {} FailurePredictStatus rows", rows.len());
        let needle_upper = pnp_device_id.to_uppercase();
        for row in &rows {
            if let Some(name) = &row.InstanceName {
                log::info!(target: "diskoria", "SMART:   row InstanceName={name:?}");
            }
        }
        for row in rows {
            let Some(name) = row.InstanceName else {
                continue;
            };
            if !name.to_uppercase().contains(&needle_upper) {
                continue;
            }
            let pred = row.PredictFailure.unwrap_or(false);
            let reason_code = row.Reason.unwrap_or(0);
            let reason = if reason_code != 0 {
                format!("SMART reason code: {reason_code}")
            } else {
                String::new()
            };
            log::info!(target: "diskoria", "SMART: matched! predict_failure={pred}, reason_code={reason_code}");
            return Some((pred, reason));
        }
        log::warn!(target: "diskoria", "SMART: no InstanceName matched needle {needle_upper:?}");
        None
    }

    pub fn query_smart_health(
        disk_number: u32,
        pnp_device_id: &str,
        device_path: &str,
        bus: crate::detected_drive::BusKind,
    ) -> Result<SmartHealth, String> {
        let wmi = WMIConnection::with_namespace_path(r"ROOT\Microsoft\Windows\Storage")
            .map_err(|e| e.to_string())?;

        let q = format!(
            "SELECT HealthStatus, OperationalStatus, OperationalDetails FROM MSFT_PhysicalDisk WHERE DeviceId = '{}'",
            disk_number
        );
        let rows: Vec<MsftPhysicalDiskRow> = wmi.raw_query(&q).map_err(|e| e.to_string())?;

        let row = rows.into_iter().next().ok_or_else(|| {
            "No storage health data for this disk (disk may be virtual or not exposed to the storage provider)."
                .to_string()
        })?;

        let health = row.HealthStatus.unwrap_or(5);
        let op_stat: Vec<u16> = row
            .OperationalStatus
            .as_ref()
            .map(variant_to_u16_vec)
            .unwrap_or_default();
        let op_details: Vec<String> = row
            .OperationalDetails
            .as_ref()
            .map(variant_to_string_vec)
            .unwrap_or_default();

        let smart_entry = failure_predict_for_disk(pnp_device_id);
        let smart_available = smart_entry.is_some();
        let (predict_failure, predict_reason) = smart_entry.unwrap_or((false, String::new()));

        let mut reason_lines: Vec<String> = op_details
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        for code in op_stat {
            if let Some(label) = operational_status_label(code) {
                let s = format!("Operational status: {}", label);
                if !reason_lines.contains(&s) {
                    reason_lines.push(s);
                }
            }
        }

        if predict_failure {
            let r = predict_reason.trim();
            if !r.is_empty() && !reason_lines.iter().any(|x| x.contains(r)) {
                reason_lines.push(r.to_string());
            } else if r.is_empty() {
                reason_lines.push("SMART reported a predictive failure.".to_string());
            }
        }

        // Deduplicate while preserving order
        let mut seen = std::collections::HashSet::new();
        reason_lines.retain(|s| seen.insert(s.clone()));

        match health {
            0 => {
                if !smart_available {
                    // The storage provider reports no issues, but this drive is
                    // not in `MSStorageDriver_FailurePredictStatus` — which
                    // enumerates ATA drives on the storage driver and nothing
                    // behind a USB bridge. Answering "Healthy" off the provider
                    // alone would still be a guess, so ask the drive directly;
                    // SAT pass-through reaches enclosures that WMI does not
                    // (KI-60). Only when that comes back empty too — a genuine
                    // flash drive, a card reader, a bridge that blocks SAT — is
                    // there no telemetry to report.
                    super::health_from_report(crate::smart_reader::query_smart_detail(
                        device_path,
                        bus,
                    ))
                    .or(Ok(SmartHealth::Disabled))
                } else if predict_failure {
                    Ok(SmartHealth::Failing {
                        reasons: if reason_lines.is_empty() {
                            vec!["SMART reported a predictive failure.".to_string()]
                        } else {
                            reason_lines
                        },
                    })
                } else {
                    Ok(SmartHealth::Healthy)
                }
            }
            1 | 2 => {
                if reason_lines.is_empty() {
                    reason_lines.push(if health == 1 {
                        "Storage provider reported a warning.".to_string()
                    } else {
                        "Storage provider reported an unhealthy state.".to_string()
                    });
                }
                Ok(SmartHealth::Failing {
                    reasons: reason_lines,
                })
            }
            5 => Ok(SmartHealth::Disabled),
            _ => Ok(SmartHealth::Disabled),
        }
    }
}

#[cfg(windows)]
pub use windows::query_smart_health;

/// Derive the verdict from the drive's own SMART data — a failed ATA
/// attribute, an NVMe critical-warning bit, or a UFS pre-EOL warning means
/// `Failing`.
///
/// This is the whole story on Linux, which has no predict-fail source. On
/// Windows it is the fallback for drives `MSStorageDriver_FailurePredictStatus`
/// does not enumerate — everything behind a USB bridge (known-issues KI-60).
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn health_from_report(
    report: crate::smart_reader::SmartReport,
) -> Result<SmartHealth, String> {
    use crate::smart_reader::{AttrStatus, SmartReport};

    match report {
        SmartReport::Ata(d) => {
            let reasons: Vec<String> = d
                .attributes
                .iter()
                .filter(|a| a.status == AttrStatus::Failed)
                .map(|a| format!("{} at/below threshold", a.name))
                .collect();
            if reasons.is_empty() {
                Ok(SmartHealth::Healthy)
            } else {
                Ok(SmartHealth::Failing { reasons })
            }
        }
        SmartReport::Nvme(d) => {
            if d.critical_warning == 0 {
                Ok(SmartHealth::Healthy)
            } else {
                Ok(SmartHealth::Failing {
                    reasons: vec![format!(
                        "NVMe critical warning 0x{:02X}",
                        d.critical_warning
                    )],
                })
            }
        }
        SmartReport::Ufs(d) => {
            if d.pre_eol_info >= 0x02 {
                Ok(SmartHealth::Failing {
                    reasons: vec![format!("UFS pre-EOL level 0x{:02X}", d.pre_eol_info)],
                })
            } else {
                Ok(SmartHealth::Healthy)
            }
        }
        SmartReport::Unavailable { reason } => Err(reason),
    }
}

/// Linux: no WMI predict-fail source exists, so the verdict comes from the same
/// SMART data the Drive Health page shows.
#[cfg(target_os = "linux")]
pub fn query_smart_health(
    _disk_number: u32,
    _pnp_device_id: &str,
    device_path: &str,
    bus: crate::detected_drive::BusKind,
) -> Result<SmartHealth, String> {
    health_from_report(crate::smart_reader::query_smart_detail(device_path, bus))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn query_smart_health(
    _disk_number: u32,
    _pnp_device_id: &str,
    _device_path: &str,
    _bus: crate::detected_drive::BusKind,
) -> Result<SmartHealth, String> {
    Err("Storage health is not implemented on this platform.".to_string())
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod hardware_tests {
    /// Diagnostic, not CI: the verdict the Sector and Benchmark pages show for
    /// every real drive. A USB drive reading `Disabled` here is KI-60.
    #[test]
    #[ignore = "inspects real hardware; run elevated with --ignored --nocapture"]
    fn print_real_smart_health() {
        let drives = crate::drive_enumeration::enumerate_physical_disks().expect("enumerate");
        for d in &drives {
            let verdict = super::query_smart_health(
                d.disk_number,
                &d.pnp_device_id,
                &d.device_id,
                d.bus,
            );
            println!(
                "#{} {:?} {} -> {:?}",
                d.disk_number,
                d.bus,
                d.model.trim(),
                verdict
            );
        }
    }
}
