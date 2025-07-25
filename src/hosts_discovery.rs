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
use chrono::{Local, NaiveDateTime, Utc};
use futures::stream::{self, StreamExt};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use tokio::time::Duration;

use crate::command_execute::run_ssh_command_on_host;

#[derive(Debug)]
struct Host {
    ip_address: String,
    ipmi_address: String,
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

fn add_host_to_db(host: Host, db_pool: &Pool<SqliteConnectionManager>) {
    let conn = db_pool.get().unwrap();
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
                    "INSERT INTO hosts (ip_address, serial, install_progress, last_updated, ipmi_address) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![host.ip_address, host.serial, host.install_progress, host.last_updated, host.ipmi_address],
                )
                .unwrap();
    } else {
        conn.execute(
                    "UPDATE hosts SET ip_address = ?1, install_progress = ?2, last_updated = ?3 , ipmi_address = ?4 WHERE serial = ?5",
                    params![host.ip_address, host.install_progress, host.last_updated, host.ipmi_address, host.serial],
                )
                .unwrap();
    }
}

// 持续监控当前 dhcp.leases 文件
pub async fn monitor_dhcp_leases(
    file_path: &str,
    interval_secs: u64,
    db_pool: Pool<SqliteConnectionManager>,
) {
    loop {
        // 记录开始时间
        let start_time = Utc::now();
        // 获取当前还在 dhcp.leases 文件且租约没到期的 IP 地址
        let active_ips = parse_dhcp_leases(file_path);
        // 并发上限
        let concurrency_limit = 10;
        // 循环所有 IP 地址进行主机获取
        stream::iter(active_ips)
            .for_each_concurrent(concurrency_limit, |ip| {
                let db_pool = db_pool.clone();
                async move {
                    // 记录当前时间
                    let current_time = Local::now()
                        .naive_local()
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string();
                    // 收集序列号信息
                    let serial = run_ssh_command_on_host(
                        &ip,
                        "cat /sys/devices/virtual/dmi/id/product_serial",
                    ).await;
                    // 当序列号收集到时，才进行后续操作，以防止浪潮读不出序列号问题
                    let serial = match serial {
                        Some(s) => s.trim().to_string(),
                        None => {
                            println!("[INFO] No serial found for IP: {}", ip);
                            return;
                        }
                    };
                    // 收集带外管理IP地址信息
                    let ipmi_addr = run_ssh_command_on_host(
                        &ip,
                        "ipmitool lan print | grep \"^IP Address\" | grep -v \"Source\" | awk '{print $4}'",
                    ).await
                    .unwrap_or("unknown".to_string());
                    // 收集安装进度信息，如果能收集到合法信息则入库
                    let install_progress =
                        run_ssh_command_on_host(&ip, "cat /tmp/install-progress").await;
                    match install_progress {
                        Some(progress) => match progress.parse::<i32>() {
                            Ok(progress) => {
                                println!(
                                    "[INFO] Install progress for IP {} ({}): {}",
                                    ip, serial, progress
                                );
                                let host = Host {
                                    ip_address: ip.clone(),
                                    ipmi_address: ipmi_addr,
                                    serial,
                                    install_progress: progress,
                                    last_updated: current_time,
                                };
                                // 入库并告诉客户端信息已收集
                                add_host_to_db(host, &db_pool);
                                run_ssh_command_on_host(
                                    &ip,
                                    &format!("echo \"{}\">/tmp/install-progress.ack", progress),
                                ).await;
                            }
                            _ => {
                                println!(
                                    "[INFO] Invalid install progress for IP {}: {}",
                                    ip, progress
                                );
                            }
                        },
                        None => {
                            println!("[INFO] No install progress found for IP: {}", ip);
                        }
                    }
                }
            })
            .await;
        // 如果当前时间与上次检查时间间隔小于指定的间隔，则等待剩余时间
        let elapsed_time = Utc::now().signed_duration_since(start_time).num_seconds();
        if elapsed_time < interval_secs as i64 {
            tokio::time::sleep(Duration::from_secs(interval_secs - elapsed_time as u64)).await;
        }
    }
}
