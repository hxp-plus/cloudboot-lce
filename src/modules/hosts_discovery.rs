use chrono::{NaiveDateTime, Utc};
use rusqlite::{Connection, params};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tokio::time::{self, Duration};

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

fn parse_dhcp_leases(file_path: &str) -> HashSet<String> {
    // Change Vec<String> to HashSet<String>
    let file = File::open(file_path).expect("Failed to open dhcpd.leases file");
    let reader = BufReader::new(file);
    let mut active_ips = HashSet::new(); // Change Vec::new() to HashSet::new()
    let mut current_lease = String::new();
    for line in reader.lines() {
        let line = line.unwrap();
        if line.split_whitespace().next() == Some("lease") {
            // Start of a new lease block
            current_lease = line.clone();
        } else if !line.trim().is_empty() {
            // Append to the current lease block
            current_lease.push_str("\n");
            current_lease.push_str(&line);
        }
        if line.trim() == "}" {
            // End of a lease block, process it
            if let Some(ip) = process_lease_block(&current_lease) {
                active_ips.insert(ip); // Use insert to add to the HashSet
            }
            current_lease.clear();
        }
    }
    active_ips
}

pub async fn monitor_dhcp_leases(
    file_path: &str,
    interval_secs: u64,
    db_pool: Arc<Mutex<Connection>>,
    ssh_password: &str,
) {
    let mut interval = time::interval(Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        let active_ips = parse_dhcp_leases(file_path);
        let conn = db_pool.lock().unwrap();
        let current_time = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(); // Get current time formatted
        for ip in &active_ips {
            // Run shell command to get the serial number for the IP address
            let output = Command::new("sshpass")
                .arg("-p")
                .arg(ssh_password)
                .arg("ssh")
                .arg("-o")
                .arg("LogLevel=ERROR")
                .arg("-o")
                .arg("UserKnownHostsFile=/dev/null")
                .arg("-o")
                .arg("ConnectTimeout=3")
                .arg(ip) // Use the IP address variable
                .arg("cat")
                .arg("/sys/devices/virtual/dmi/id/product_serial")
                .output();
            let serial = match output {
                Ok(output) if output.status.success() => {
                    String::from_utf8_lossy(&output.stdout).trim().to_string()
                }
                _ => {
                    println!("Failed to retrieve serial for IP: {}", ip);
                    continue;
                }
            };
            // Run shell command to get the install progress number for the IP address
            let output = Command::new("sshpass")
                .arg("-p")
                .arg(ssh_password)
                .arg("ssh")
                .arg("-o")
                .arg("LogLevel=ERROR")
                .arg("-o")
                .arg("UserKnownHostsFile=/dev/null")
                .arg("-o")
                .arg("ConnectTimeout=3")
                .arg(ip) // Use the IP address variable
                .arg("cat")
                .arg("/tmp/install-progress")
                .output();
            let progress_output = match output {
                Ok(output) if output.status.success() => {
                    String::from_utf8_lossy(&output.stdout).trim().to_string()
                }
                _ => {
                    println!("Failed to retrieve install progress for IP: {}", ip);
                    String::from("-1")
                }
            };
            // Convert the progress string to an integer
            let progress_int: Result<i32, _> = progress_output.parse();
            let progress = match progress_int {
                Ok(value) => value,
                Err(_) => {
                    println!("Failed to convert progress to integer for IP: {}", ip);
                    -1
                }
            };
            // Check if the serial already exists in the hosts table
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM hosts WHERE serial = ?1)",
                    params![serial],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            // If the serial doesn't exist, insert it into the hosts table
            if !exists {
                conn.execute(
                    "INSERT INTO hosts (ip_address, serial, install_progress, last_updated) VALUES (?1, ?2, ?3, ?4)",
                    params![ip, serial, progress, current_time],
                )
                .unwrap();
            } else {
                // Update the last_updated column for existing IPs
                conn.execute(
                    "UPDATE hosts SET ip_address = ?1, install_progress = ?2, last_updated = ?3 WHERE serial = ?4",
                    params![ip, progress, current_time, serial],
                )
                .unwrap();
            }
        }
        println!("Updated hosts table with active IPs: {:?}", active_ips);
    }
}
