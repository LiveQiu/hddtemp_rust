use rayon::prelude::*;
use serde_json::{from_str, Value};
use std::io;
use std::process::{Command, Output};

// 根据设备类型尝试不同的 smartctl 参数
const DEVICE_TYPES: [&str; 5] = ["", "ata", "sat", "scsi", "nvme"]; // 空字符串代表默认模式

// 解析 smartctl 输出，将硬盘信息分为厂商名和型号，并获取温度
fn parse_smartctl_output(output: &Output) -> io::Result<(String, String, Option<i64>)> {
    let output_str = String::from_utf8_lossy(&output.stdout);

    // 尝试解析 JSON 格式的输出
    let json_data: Value = match from_str(&output_str) {
        Ok(data) => data,
        Err(e) => {
            // 如果 JSON 解析失败，尝试从原始输出中提取信息
            if let Some(temp) = extract_temperature_from_text(&output_str) {
                return Ok((
                    "Unknown Vendor".to_string(),
                    "Unknown Model".to_string(),
                    Some(temp),
                ));
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to parse smartctl output: {}", e),
            ));
        }
    };

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
        .or_else(|| json_data["temperature"].as_i64())
        .or_else(|| json_data["nvme_smart_health_information_log"]["temperature"].as_i64())
        .or_else(|| {
            json_data["ata_smart_attributes"]["table"]
                .as_array()
                .and_then(|attributes| {
                    attributes
                        .iter()
                        .filter_map(|attr| {
                            let name = attr["name"].as_str()?.to_lowercase();
                            if name.contains("temperature") || name.contains("temp") {
                                attr["raw"]["value"]
                                    .as_i64()
                                    .or_else(|| attr["value"].as_i64())
                            } else {
                                None
                            }
                        })
                        .next()
                })
        });

    Ok((vendor_part, model_part, temperature))
}

// 从文本输出中提取温度（备用方法）
fn extract_temperature_from_text(output: &str) -> Option<i64> {
    // 尝试匹配常见的温度格式
    for line in output.lines() {
        if line.contains("Temperature") || line.contains("Airflow_Temperature") {
            if let Some(temp) = line
                .split_whitespace()
                .filter_map(|word| word.parse::<i64>().ok())
                .find(|&t| t > 0 && t < 150)
            {
                return Some(temp);
            }
        }
    }
    None
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
                if !device_path.starts_with("/dev/zd") && !device_path.starts_with("/dev/fd") {
                    return Some(device_path);
                }
            }
            None
        })
        .collect();

    Ok(devices)
}

// 尝试为每个设备调用 smartctl 并自动切换 -d 参数
fn get_disk_info_and_temperature(device: &str) -> io::Result<(String, String, Option<i64>)> {
    // 遍历支持的 DEVICE_TYPES 模式
    for device_type in DEVICE_TYPES.iter() {
        // 构造 smartctl 命令参数
        let mut args = vec!["--json", "-a"];
        if !device_type.is_empty() {
            args.push("-d");
            args.push(device_type);
        }
        args.push(device);

        // 添加超时机制防止卡住
        let output = match Command::new("smartctl")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => {
                let output = match child.wait_with_output() {
                    Ok(output) => output,
                    Err(e) => {
                        eprintln!("Command execution failed for {}: {}", device, e);
                        continue;
                    }
                };
                output
            }
            Err(e) => {
                eprintln!("Failed to spawn smartctl for {}: {}", device, e);
                continue;
            }
        };

        // 检查输出是否有效
        if !output.stdout.is_empty() {
            // 即使命令返回非0状态码，也尝试解析输出
            match parse_smartctl_output(&output) {
                Ok(info) => return Ok(info),
                Err(e) => {
                    eprintln!(
                        "Failed to parse output for {} (type {}): {}",
                        device, device_type, e
                    );
                    continue;
                }
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::Other,
        format!("All attempts failed for device: {}", device),
    ))
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
        .map(|device| {
            match get_disk_info_and_temperature(device) {
                Ok((vendor, model, temp)) => (
                    device.to_string(),
                    vendor,
                    model,
                    temp.map_or("N/A".to_string(), |t| format!("{:3}°C", t)),
                    "OK".to_string(),
                ),
                Err(e) => {
                    // 尝试直接运行 smartctl 获取原始输出用于调试
                    let raw_output = Command::new("smartctl")
                        .arg("-a")
                        .arg(device)
                        .output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                        .unwrap_or_else(|_| "Failed to get raw output".to_string());

                    eprintln!("Debug raw output for {}:\n{}", device, raw_output);

                    (
                        device.to_string(),
                        "Failed".to_string(),
                        "Failed".to_string(),
                        e.to_string(),
                        "FAIL".to_string(),
                    )
                }
            }
        })
        .collect();

    // 计算每列的最大宽度
    let (max_device_len, max_vendor_len, max_model_len, max_temp_len) = results.iter().fold(
        (0, 0, 0, 0),
        |(d_max, v_max, m_max, t_max), (d, v, m, t, _)| {
            (
                d_max.max(d.len()),
                v_max.max(v.len()),
                m_max.max(m.len()),
                t_max.max(t.len()),
            )
        },
    );

    // 打印表头
    println!(
        "{:<device_width$} {:<vendor_width$} {:<model_width$} {:<temp_width$} STATUS",
        "DEVICE",
        "VENDOR",
        "MODEL",
        "TEMP",
        device_width = max_device_len,
        vendor_width = max_vendor_len,
        model_width = max_model_len,
        temp_width = max_temp_len,
    );

    // 打印结果并对齐
    for (device, vendor, model, temp, status) in results {
        println!(
            "{:<device_width$} {:<vendor_width$} {:<model_width$} {:<temp_width$} {}",
            device,
            vendor,
            model,
            temp,
            status,
            device_width = max_device_len,
            vendor_width = max_vendor_len,
            model_width = max_model_len,
            temp_width = max_temp_len,
        );
    }
}
