use rayon::prelude::*;
use serde_json::{from_str, Value};
// use std::borrow::Cow;
use std::io;
use std::process::{Command, Output};

// 解析 smartctl 输出，将硬盘信息分为厂商名和型号，并获取温度
fn parse_smartctl_output(output: &Output) -> io::Result<(String, String, Option<i64>)> {
    let output_str = String::from_utf8_lossy(&output.stdout);

    // 尝试解析 JSON 格式的输出
    let json_data: Value = from_str(&output_str).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to parse smartctl JSON output: {}", e),
        )
    })?;

    // 提取厂商名、硬件型号或猜测值
    let vendor = json_data["scsi_vendor"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let product = json_data["scsi_product"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let model = json_data["model_name"]
        .as_str()
        .or_else(|| json_data["model_family"].as_str())
        .or_else(|| json_data["scsi_model_name"].as_str())
        .unwrap_or("Unknown Model")
        .to_owned();

    let (vendor_part, model_part) = if !vendor.is_empty() && !product.is_empty() {
        (vendor, product)
    } else {
        // 尝试拆分 model 到厂商和型号两部分
        let mut parts = model.split_whitespace();
        let vendor_guess = parts.next().unwrap_or("Unknown Vendor").to_string();
        let model_guess = parts.collect::<Vec<&str>>().join(" ");
        (vendor_guess, model_guess)
    };

    // 提取温度信息（按优先顺序查询可能的字段）
    let temperature = json_data["temperature"]["current"]
        .as_i64()
        .or_else(|| json_data["nvme_smart_health_information_log"]["temperature"].as_i64())
        .or_else(|| {
            json_data["ata_smart_attributes"]["table"]
                .as_array()
                .and_then(|attributes| {
                    attributes
                        .iter()
                        .filter_map(|attr| {
                            let name = attr["name"].as_str()?.to_lowercase();
                            if name.contains("temperature") {
                                attr["raw"]["value"].as_i64()
                            } else {
                                None
                            }
                        })
                        .next()
                })
        });

    Ok((vendor_part, model_part, temperature))
}

// 获取系统中所有硬盘设备
fn get_all_disk_devices() -> io::Result<Vec<String>> {
    let output = Command::new("lsblk")
        .arg("-d")
        .arg("-o")
        .arg("NAME,TYPE")
        .arg("-n")
        .arg("-l")
        .output()?;

    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "lsblk command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }

    // 解析输出获取设备列表
    let devices = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == "disk" {
                let device_path = format!("/dev/{}", parts[0]);
                if !device_path.starts_with("/dev/zd") {
                    return Some(device_path);
                }
            }
            None
        })
        .collect();

    Ok(devices)
}

// 获取硬盘的厂商名、型号和温度
fn get_disk_info_and_temperature(device: &str) -> io::Result<(String, String, Option<i64>)> {
    let output = Command::new("smartctl")
        .args(&["--json", "-a", device])
        .output()?;

    if !output.status.success() || output.stdout.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "smartctl command failed for {}: {}",
                device,
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }

    parse_smartctl_output(&output)
}

// 主函数
fn main() {
    // 检查是否有 root 权限
    if !nix::unistd::Uid::effective().is_root() {
        eprintln!("Error: This program must be run as root (use sudo)");
        std::process::exit(1);
    }

    // 获取硬盘设备列表
    let devices = match get_all_disk_devices() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to get disk devices: {}", e);
            std::process::exit(1);
        }
    };

    if devices.is_empty() {
        eprintln!("No disk devices found.");
        return;
    }

    println!("Detected disk devices:\n");

    // 并行处理每个设备，获取厂商名、硬盘型号和温度
    let results: Vec<_> = devices
        .par_iter()
        .map(|device| match get_disk_info_and_temperature(device) {
            Ok((vendor, model, temp)) => (
                device.to_string(),
                vendor,
                model,
                temp.map_or("N/A".to_string(), |t| format!("{:3}°C", t)),
            ),
            Err(e) => (
                device.to_string(),
                "Failed".to_string(),
                "Failed".to_string(),
                e.to_string(),
            ),
        })
        .collect();

    // 计算每列的最大宽度
    let (max_device_len, max_vendor_len, max_model_len, max_temp_len) = results.iter().fold(
        (0, 0, 0, 0),
        |(d_max, v_max, m_max, t_max), (d, v, m, t)| {
            (
                d_max.max(d.len()),
                v_max.max(v.len()),
                m_max.max(m.len()),
                t_max.max(t.len()),
            )
        },
    );

    // 打印结果并对齐
    for (device, vendor, model, temp) in results {
        println!(
            "{:<device_width$} {:<vendor_width$} {:<model_width$} {:<temp_width$}",
            device,
            vendor,
            model,
            temp,
            device_width = max_device_len,
            vendor_width = max_vendor_len,
            model_width = max_model_len,
            temp_width = max_temp_len,
        );
    }
}
