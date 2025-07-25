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

// 主机装机进度控制代码：这段代码用于监控主机上 /tmp/install-progress.ack 文件并做相应的处理
use chrono::Utc;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use tokio::process::Command;
use tokio::time::Duration;

use crate::command_execute::run_ssh_command_on_host;

struct Host {
    ip_address: String,
    os: String,
    hostname: String,
    public_ip_addr: String,
    vlan_id: u32,
}

pub enum Progress {
    NotConfigured = 0,
    RebootingToKickstart = 5,
    KickstartLoaded = 10,
    PreInstallFinished = 20,
    PostInstallFinished = 60,
    InstallFinished = 80,
    RebootedToSystem = 85,
}

// 将所有尚未开始安装但是配置了操作系统的机器，安装进度置为正在重启到kickstart
async fn start_kickstart_installation(db_pool: Pool<SqliteConnectionManager>) {
    let hosts_to_process = tokio::task::spawn_blocking(move || {
        let conn = db_pool.get().unwrap();
        // 查询安装进度为NotConfigured且os不为空的主机
        let mut stmt = conn
            .prepare(
                "SELECT ip_address, os, hostname, public_ip_addr, vlan_id FROM hosts WHERE install_progress = ?1 AND os IS NOT NULL",
            )
            .unwrap();
        let host_iter = stmt
            .query_map(params![Progress::NotConfigured as i32], |row| {
                Ok(Host {
                    ip_address: row.get(0)?,
                    os: row.get(1)?,
                    hostname: row.get(2)?,
                    public_ip_addr: row.get(3)?,
                    vlan_id: row.get(4)?,
                })
            })
            .unwrap();
        let mut hosts: Vec<Host> = host_iter.filter_map(Result::ok).collect();
        // 检查每个主机的os是否在ipxe表且ipxe表里script不为空
        let mut hosts_with_ipxe: Vec<Host> = Vec::new();
        for host in hosts.drain(..) {
            let mut ipxe_stmt = conn
                .prepare("SELECT script FROM ipxe WHERE os = ?1 AND script IS NOT NULL")
                .unwrap();
            let script_exists = ipxe_stmt
                .query_map(params![host.os], |row| row.get::<_, String>(0))
                .unwrap()
                .count()
                > 0;
            if script_exists {
                hosts_with_ipxe.push(host);
            }
        }
        hosts_with_ipxe
    })
    .await
    .expect("Failed to get hosts from database");
    for host in hosts_to_process {
        run_ssh_command_on_host(
            &host.ip_address,
            &format!(
                "echo \"{}\" >/tmp/install-progress",
                Progress::RebootingToKickstart as i32
            ),
        )
        .await;
        println!(
            "[INFO] Setting host {} install progress to: RebootingToKickstart",
            host.ip_address
        );
    }
}

// 将所有安装进度为 RebootingToKickstart 的主机重启到 kickstart
async fn reboot_host_to_kickstart(db_pool: Pool<SqliteConnectionManager>) {
    // 将数据库操作移动到阻塞线程
    let hosts: Vec<Host> = tokio::task::spawn_blocking(move || {
        let conn = db_pool.get().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT ip_address, os, hostname, public_ip_addr, vlan_id FROM hosts WHERE install_progress = ?1 AND os IS NOT NULL",
            )
            .unwrap();
        stmt.query_map(params![Progress::RebootingToKickstart as i32], |row| {
            Ok(Host {
                ip_address: row.get(0)?,
                os: row.get(1)?,
                hostname: row.get(2)?,
                public_ip_addr: row.get(3)?,
                vlan_id: row.get(4)?,
            })
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect()
    })
    .await
    .unwrap();
    // 检查每个主机的 /tmp/install-progress.ack 文件是否为 RebootingToKickstart
    for host in hosts {
        let progress_on_host =
            run_ssh_command_on_host(&host.ip_address, &format!("cat /tmp/install-progress.ack"))
                .await
                .unwrap_or("".to_string());
        if progress_on_host.trim() == (Progress::RebootingToKickstart as i32).to_string() {
            run_ssh_command_on_host(
                &host.ip_address,
                "ipmitool chassis bootdev pxe;/sbin/reboot",
            )
            .await;
            println!("[INFO] Rebooting host: {}", host.ip_address);
        }
    }
}

// 将已经装好重启完毕的机器配置主机名和网络
async fn configure_host_after_installation(db_pool: Pool<SqliteConnectionManager>) {
    // 将数据库操作移动到阻塞线程
    let hosts: Vec<Host> = tokio::task::spawn_blocking(move || {
        let conn = db_pool.get().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT ip_address, os, hostname, public_ip_addr, vlan_id FROM hosts WHERE install_progress = ?1 AND os IS NOT NULL",
            )
            .unwrap();
        stmt.query_map(params![Progress::RebootedToSystem as i32], |row| {
            Ok(Host {
                ip_address: row.get(0)?,
                os: row.get(1)?,
                hostname: row.get(2)?,
                public_ip_addr: row.get(3)?,
                vlan_id: row.get(4)?,
            })
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect()
    })
    .await
    .unwrap();
    // 对所有的机器进行网络配置
    for host in hosts {
        // 获取主机所有网卡
        let nics = run_ssh_command_on_host(
            &host.ip_address,
            r#"
            for dev in /sys/class/net/*/uevent; do
                nic=$(cat ${dev} | grep INTERFACE | awk -F'=' '{print $2}')
                port=$(ethtool ${nic} | awk '/Port/ {print$NF}')
                link=$(ethtool ${nic} | awk '/Link/ {print$NF}')
                [[ "$port" == "FIBRE" ]] && [[ "$nic" != "lo" ]] && echo "$nic"
            done
            "#,
        )
        .await;
        if let Some(nics) = nics {
            // 判断网卡数量是否为2
            if nics.lines().count() != 2 {
                println!(
                    "[WARN] Host {} has {} NICs, expected 2 NICs for configuration.",
                    host.ip_address,
                    nics.lines().count()
                );
                continue;
            } else {
                let hostname = host.hostname;
                let nic_1 = nics.lines().nth(0).unwrap().trim();
                let nic_2 = nics.lines().nth(1).unwrap().trim();
                let public_ip_addr = host.public_ip_addr;
                let gateway = public_ip_addr
                    .split('.')
                    .take(3)
                    .collect::<Vec<&str>>()
                    .join(".")
                    + ".1";
                let vlan_id = host.vlan_id;
                run_ssh_command_on_host(&host.ip_address, &format!("
                    mkdir -p /tmp/.install
                    cat >/tmp/.install/network-config.sh <<EOF
                        #!/bin/bash
                        hostnamectl set-hostname --static {hostname}
                        rm -f /etc/sysconfig/network-scripts/ifcfg-*
                        nmcli -t -f uuid connection show | xargs nmcli connection delete
                        nmcli connection add type bond ifname bond0 con-name bond0 mode 4 ipv4.method disabled ipv6.method ignore ipv6.addr-gen-mode eui64
                        nmcli connection add type bond-slave ifname {nic_1} con-name {nic_1} master bond0
                        nmcli connection add type bond-slave ifname {nic_2} con-name {nic_2} master bond0
                        nmcli connection up bond0
                        nmcli con add type vlan ifname bond0.{vlan_id} con-name bond0.{vlan_id} id {vlan_id} dev bond0
                        nmcli connection modify bond0.{vlan_id} ipv4.method manual ipv4.addresses {public_ip_addr}/24
                        nmcli connection modify bond0.{vlan_id} ipv4.gateway {gateway}
                        nmcli connection up bond0.{vlan_id}
                        nmcli connection reload
                        ping -c10 {public_ip_addr}
                        nmcli connection show
                        cat /proc/net/bonding/bond0 | grep -i agger
                    EOF
                    chmod +x /tmp/.install/network-config.sh
                    "
                )).await;
                println!(
                    "[INFO] Host {} configured with network and hostname.",
                    host.ip_address
                );
                // ping主机公网IP，如果通，将安装进度设置为安装完成
                let mut command = Command::new("ping");
                command.arg("-c").arg("1").arg("-W").arg("1");
                let result = command.output().await;
                match result {
                    Ok(output) => {
                        if output.status.success() {
                            println!("[INFO] Ping to {} successful!", &public_ip_addr);
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            println!(
                                "[INFO] Ping to {} failed with non-zero exit code. Stderr: {}",
                                &public_ip_addr, stderr
                            );
                        }
                    }
                    Err(e) => {
                        println!("[ERROR] Failed to execute ping command: {}", e);
                    }
                }
            }
        } else {
            println!("[WARN] No NICs found for host: {}", host.ip_address);
        }
    }
}

// 持续监控主机状态，并在达到进度时下发操作
pub async fn progress_control(interval_secs: u64, db_pool: Pool<SqliteConnectionManager>) {
    loop {
        // 记录开始时间
        let start_time = Utc::now();
        // 将所有满足装机条件的机器状态设置为RebootingToKickstart
        start_kickstart_installation(db_pool.clone()).await;
        // 重启所有状态为RebootingToKickstart的机器
        reboot_host_to_kickstart(db_pool.clone()).await;
        // 配置所有已经装机完成的机器
        configure_host_after_installation(db_pool.clone()).await;
        // 如果当前时间与上次检查时间间隔小于指定的间隔，则等待剩余时间
        let elapsed_time = Utc::now().signed_duration_since(start_time).num_seconds();
        if elapsed_time < interval_secs as i64 {
            tokio::time::sleep(Duration::from_secs(interval_secs - elapsed_time as u64)).await;
        }
    }
}
