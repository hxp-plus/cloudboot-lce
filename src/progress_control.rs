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
    hostname: String,
    public_ip_addr: String,
    ipmi_address: String,
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
    let db_pool_clone = db_pool.clone();
    let hosts_to_process = tokio::task::spawn_blocking(move || {
        let conn = db_pool.get().unwrap();
        // 查询 install_queue 表，并与 hosts 表进行 LEFT JOIN
        // 同时在 SQL 查询中检查 ipxe 表是否存在相应的脚本
        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    h.ip_address,
                    h.hostname,
                    h.public_ip_addr,
                    h.vlan_id,
                    iq.ipmi_address
                FROM install_queue iq
                LEFT JOIN hosts h ON iq.ipmi_address = h.ipmi_address
                WHERE h.install_progress = ?1
                  AND h.os IS NOT NULL
                  AND EXISTS (SELECT 1 FROM ipxe WHERE os = h.os AND script IS NOT NULL)
                "#,
            )
            .unwrap();
        let host_iter = stmt
            .query_map(params![Progress::NotConfigured as i32], |row| {
                Ok(Host {
                    ip_address: row.get(0)?,
                    hostname: row.get(1)?,
                    public_ip_addr: row.get(2)?,
                    vlan_id: row.get(3)?,
                    ipmi_address: row.get(4)?, // 获取 ipmi_address
                })
            })
            .unwrap();
        let hosts: Vec<Host> = host_iter.filter_map(Result::ok).collect();
        hosts
    })
    .await
    .expect("Failed to get hosts from database");
    for host in hosts_to_process {
        // 更新主机状态到 RebootingToKickstart
        run_ssh_command_on_host(
            &host.ip_address,
            &format!(
                "echo \"{}\" >/tmp/install-progress",
                Progress::RebootingToKickstart as i32
            ),
        )
        .await;
        println!(
            "[INFO] Setting host {} (IPMI: {}) install progress to: RebootingToKickstart",
            host.ip_address, host.ipmi_address
        );
        // SSH 命令成功后，删除 install_queue 中的 ipmi_address
        let ipmi_address = host.ipmi_address.clone();
        let conn = db_pool_clone.get().unwrap();
        conn.execute(
            "DELETE FROM install_queue WHERE ipmi_address = ?1",
            params![ipmi_address],
        )
        .unwrap();
    }
}

// 将所有安装进度为 RebootingToKickstart 的主机重启到 kickstart
async fn reboot_host_to_kickstart(db_pool: Pool<SqliteConnectionManager>) {
    // 将数据库操作移动到阻塞线程
    let hosts: Vec<Host> = tokio::task::spawn_blocking(move || {
        let conn = db_pool.get().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT ip_address, hostname, public_ip_addr, vlan_id, ipmi_address FROM hosts WHERE install_progress = ?1 AND os IS NOT NULL",
            )
            .unwrap();
        stmt.query_map(params![Progress::RebootingToKickstart as i32], |row| {
            Ok(Host {
                ip_address: row.get(0)?,
                hostname: row.get(1)?,
                public_ip_addr: row.get(2)?,
                vlan_id: row.get(3)?,
                ipmi_address: row.get(4)?,
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
                "ipmitool chassis bootdev pxe options=efiboot;/sbin/reboot",
            )
            .await;
            println!("[INFO] Rebooting host: {}", host.ip_address);
        }
    }
}

// 将已经装好重启完毕的机器配置主机名和网络
async fn configure_host_after_installation(db_pool: Pool<SqliteConnectionManager>) {
    let db_pool_clone = db_pool.clone();
    // 将数据库操作移动到阻塞线程
    let hosts: Vec<Host> = tokio::task::spawn_blocking(move || {
        let conn = db_pool.get().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT ip_address, hostname, public_ip_addr, vlan_id, ipmi_address FROM hosts WHERE install_progress = ?1 AND os IS NOT NULL",
            )
            .unwrap();
        stmt.query_map(params![Progress::RebootedToSystem as i32], |row| {
            Ok(Host {
                ip_address: row.get(0)?,
                hostname: row.get(1)?,
                public_ip_addr: row.get(2)?,
                vlan_id: row.get(3)?,
                ipmi_address: row.get(4)?,
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
            echo
            "#,
        )
        .await;
        if let Some(nics) = nics {
            let hostname = host.hostname;
            let nic_1 = nics.lines().nth(0).unwrap().trim();
            let nic_2 = match nics.lines().count() {
                4 => nics.lines().nth(2).map(|s| s.trim()).unwrap(),
                2 => nics.lines().nth(1).map(|s| s.trim()).unwrap(),
                _ => {
                    println!(
                        "[ERROR] Unexpected number of NICs for host {}: {}",
                        host.ip_address,
                        nics.lines().count()
                    );
                    continue;
                }
            };
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
                    cat >/tmp/.install/network-config.sh <<-'EOF'
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
                        cat /proc/net/bonding/bond0 | grep Aggregator
                    "
                )).await;
            run_ssh_command_on_host(&host.ip_address, "sed -i 's/^[[:space:]]*//' /tmp/.install/network-config.sh;chmod +x /tmp/.install/network-config.sh").await;
            run_ssh_command_on_host(
                &host.ip_address,
                "nohup /tmp/.install/network-config.sh &>/tmp/.install/network-config.log &",
            )
            .await;
            println!(
                "[INFO] Host {} configured with network and hostname.",
                host.ip_address
            );
        } else {
            // ping主机公网IP，如果通，将安装进度设置为安装完成
            let mut command = Command::new("ping");
            command
                .arg("-c")
                .arg("1")
                .arg("-W")
                .arg("1")
                .arg(&host.public_ip_addr);
            let result = command.output().await;
            match result {
                Ok(output) => {
                    if output.status.success() {
                        println!("[INFO] Ping to {} successful!", &host.public_ip_addr);
                        let conn = db_pool_clone.get().unwrap();
                        conn.execute(
                            "UPDATE hosts SET install_progress = ?1 WHERE public_ip_addr = ?2",
                            params![100, &host.public_ip_addr],
                        )
                        .unwrap();
                    } else {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        println!(
                            "[INFO] Ping to {} failed with non-zero exit code. stdout: {}, stderr: {}",
                            &host.public_ip_addr, stdout, stderr
                        );
                    }
                }
                Err(e) => {
                    println!("[ERROR] Failed to execute ping command: {}", e);
                }
            }
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
