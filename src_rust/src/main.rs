#[macro_use] // 启用 prettytable 宏
extern crate prettytable;

use prettytable::{format, Cell, Row, Table};
use rayon::prelude::*;
use serde_json::{from_str, Value};
use std::io;
use std::process::{Command, Output};

// 根据设备类型尝试不同的 smartctl 参数
const DEVICE_TYPES: [&str; 6] = ["", "ata", "sat", "scsi", "nvme", "sata"]; // 增加了"sata"类型

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

    // 提取厂商名 - 优先从model_family中提取（适用于SATA硬盘）
    let vendor = if let Some(model_family) = json_data["model_family"].as_str() {
        // 尝试从model_family中提取厂商名（通常是第一个单词）
        model_family
            .split_whitespace()
            .next()
            .unwrap_or("Unknown Vendor")
            .to_string()
    } else {
        json_data["vendor"]
            .as_str()
            .or_else(|| json_data["scsi_vendor"].as_str())
            .unwrap_or("Unknown Vendor")
            .to_string()
    };

    // 提取模型名
    let model = json_data["model_name"]
        .as_str()
        .or_else(|| json_data["product"].as_str()) // 对于SATA设备可能使用product字段
        .or_else(|| json_data["scsi_product"].as_str())
        .or_else(|| json_data["scsi_model_name"].as_str())
        .unwrap_or("Unknown Model")
        .to_string();

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
        })
        .or_else(|| json_data["sata_temperature"].as_i64()); // 添加SATA特定温度字段

    Ok((vendor, model, temperature))
}

// 从文本输出中提取温度（备用方法）
fn extract_temperature_from_text(output: &str) -> Option<i64> {
    // 尝试匹配常见的温度格式
    for line in output.lines() {
        if line.to_lowercase().contains("temperature")
            || line.to_lowercase().contains("airflow_temperature")
            || line.to_lowercase().contains("temp")
        {
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
    // 首先尝试不带任何设备类型参数（适用于大多数SATA设备）
    let mut args = vec!["--json", "-a", device];
    let output = execute_smartctl(&args);
    if let Ok(info) = parse_smartctl_output(&output) {
        return Ok(info);
    }

    // 如果默认方式失败，尝试所有设备类型
    for device_type in DEVICE_TYPES.iter().filter(|&&t| !t.is_empty()) {
        args = vec!["--json", "-a", "-d", device_type, device];
        let output = execute_smartctl(&args);
        if let Ok(info) = parse_smartctl_output(&output) {
            return Ok(info);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::Other,
        format!("Failed for device: {}", device),
    ))
}

// 执行smartctl命令的辅助函数
fn execute_smartctl(args: &[&str]) -> Output {
    Command::new("smartctl")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|child| child.wait_with_output())
        .unwrap_or_else(|e| {
            eprintln!("Failed to execute smartctl with args {:?}: {}", args, e);
            Output {
                status: std::process::ExitStatus::default(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        })
}

// 主函数
fn main() {
    // 检查是否有 root 权限
    if !nix::unistd::Uid::effective().is_root() {
        eprintln!("Must be run as root.");
        std::process::exit(1);
    }

    // 获取硬盘设备列表
    let devices = match get_all_disk_devices() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to get devices: {e}");
            std::process::exit(1);
        }
    };

    // println!("Disk devices:");

    // 并行处理每个设备，获取厂商名、硬盘型号和温度
    let results: Vec<_> = devices
        .par_iter()
        .map(|device| match get_disk_info_and_temperature(device) {
            Ok((vendor, model, temp)) => (
                device.to_string(),
                vendor,
                model,
                temp.map_or("N/A".to_string(), |t| format!("{t}°C")),
                "OK".to_string(),
            ),
            Err(e) => (
                device.to_string(),
                "Failed".to_string(),
                "Failed".to_string(),
                e.to_string(),
                "FAIL".to_string(),
            ),
        })
        .collect();

    let mut table = Table::new();
    table.set_format(
        format::FormatBuilder::new()
            .padding(2, 2) // 设置左右填充空格
            .build(),
    );
    table.add_row(row!["DEVICE", "VENDOR", "MODEL", "TEMP", "STATUS"]);
    for (device, vendor, model, temp, status) in results {
        table.add_row(Row::new(vec![
            Cell::new(&device),
            Cell::new(&vendor),
            Cell::new(&model),
            Cell::new(&temp),
            Cell::new(&status),
        ]));
    }

    table.printstd();
}
