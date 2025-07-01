/*
 * Copyright 2025 Xiping Hu <hxp@hxp.plus>
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *    http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
*/

// 主机发现相关代码：这段代码用于监控 dhcp.leases 并对所有有 dhcp 租约的主机进行信息更新

use chrono::{NaiveDateTime, Utc};
use rusqlite::{Connection, params};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};
use tokio::time::{self, Duration};

use crate::command_execute::run_ssh_command_on_host;

#[derive(Debug)]
struct Host {
    ip_address: String,
    serial: String,
    install_progress: i32,
    last_updated: String,
}

// 处理 dhcp.leases 里每一个 lease 块
fn process_lease_block(lease_block: &str) -> Option<String> {
    let mut ip_address = None;
    let mut ends_timestamp = None;
    for line in lease_block.lines() {
        if line.split_whitespace().next() == Some("lease") {
            ip_address = line.split_whitespace().nth(1).map(String::from);
        } else if line.split_whitespace().next() == Some("ends") {
            let timestamp_str = line
                .split_whitespace()
                .skip(2)
                .collect::<Vec<_>>()
                .join(" ");
            ends_timestamp =
                NaiveDateTime::parse_from_str(&timestamp_str, "%Y/%m/%d %H:%M:%S;").ok();
        }
    }
    if let (Some(ip), Some(ends)) = (ip_address, ends_timestamp) {
        let current_time = Utc::now().naive_utc();
        if ends > current_time {
            println!("[DEBUG] found valid lease block: \n{}", lease_block);
            return Some(ip);
        }
    }
    None
}

// 找到所有 dhcp.leases 里 lease 块并解析
fn parse_dhcp_leases(file_path: &str) -> HashSet<String> {
    let file = File::open(file_path).expect("Failed to open dhcpd.leases file");
    let reader = BufReader::new(file);
    let mut active_ips = HashSet::new();
    // current_lease 用于存储正在被解析的 lease 块
    let mut current_lease = String::new();
    for line in reader.lines() {
        // 读取当前行
        let line = line.unwrap();
        // 如果当前行的以 lease 开头则将其作为 current_lease 第一行，如果当前行不是空行，则将其添加到 current_lease 中
        if line.split_whitespace().next() == Some("lease") {
            current_lease = line.clone();
        } else if !line.trim().is_empty() {
            current_lease.push_str("\n");
            current_lease.push_str(&line);
        }
        // 如果当前行是右括号，则 current_lease 里内容为当前完整 lease 块
        if line.trim() == "}" {
            // 处理当前 lease 如果有 IP 地址且未过期，则将其加入 active_ips
            if let Some(ip) = process_lease_block(&current_lease) {
                active_ips.insert(ip);
            }
            // 清空当前 current_lease
            current_lease.clear();
        }
    }
    active_ips
}

fn add_host_to_db(host: Host, db_pool: &Arc<Mutex<Connection>>) {
    let conn = db_pool.lock().unwrap();
    // 检查序列号是否存在
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM hosts WHERE serial = ?1)",
            params![host.serial],
            |row| row.get(0),
        )
        .unwrap_or(false);
    // 如果序列号不存在则插入，否则更新
    if !exists {
        conn.execute(
                    "INSERT INTO hosts (ip_address, serial, install_progress, last_updated) VALUES (?1, ?2, ?3, ?4)",
                    params![host.ip_address, host.serial, host.install_progress, host.last_updated],
                )
                .unwrap();
    } else {
        conn.execute(
                    "UPDATE hosts SET ip_address = ?1, install_progress = ?2, last_updated = ?3 WHERE serial = ?4",
                    params![host.ip_address, host.install_progress, host.last_updated, host.serial],
                )
                .unwrap();
    }
}

// 持续监控当前 dhcp.leases 文件
pub async fn monitor_dhcp_leases(
    file_path: &str,
    interval_secs: u64,
    db_pool: Arc<Mutex<Connection>>,
) {
    let mut interval = time::interval(Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        // 获取当前还在 dhcp.leases 文件且租约没到期的 IP 地址
        let active_ips = parse_dhcp_leases(file_path);
        // 循环所有 IP 地址进行主机获取
        for ip in &active_ips {
            let current_time = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let serial =
                run_ssh_command_on_host(&ip, "cat /sys/devices/virtual/dmi/id/product_serial")
                    .unwrap_or("unknown".to_string());
            let install_progress = run_ssh_command_on_host(&ip, "cat /tmp/install-progress");
            match install_progress {
                Some(progress) => match progress.parse::<i32>() {
                    Ok(progress) => {
                        println!("[DEBUG] Install progress for IP {}: {}", ip, progress);
                        let host = Host {
                            ip_address: ip.clone(),
                            serial,
                            install_progress: progress,
                            last_updated: current_time,
                        };
                        add_host_to_db(host, &db_pool);
                    }
                    _ => {
                        println!(
                            "[DEBUG] Invalid install progress for IP {}: {}",
                            ip, progress
                        );
                    }
                },
                None => {
                    println!("[DEBUG] No install progress found for IP: {}", ip);
                }
            }
        }
    }
}
