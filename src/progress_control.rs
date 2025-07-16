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
use tokio::time::Duration;

use crate::command_execute::run_ssh_command_on_host;

struct Host {
    ip_address: String,
    os: String,
}

pub enum Progress {
    NotConfigured = 0,
    RebootingToKickstart = 5,
    KickstartLoaded = 10,
    PreInstallFinished = 20,
    PostInstallFinished = 60,
    InstallFinished = 100,
}

// 将所有尚未开始安装但是配置了操作系统的机器，安装进度置为正在重启到kickstart
async fn start_kickstart_installation(db_pool: Pool<SqliteConnectionManager>) {
    let hosts_to_process = tokio::task::spawn_blocking(move || {
        let conn = db_pool.get().unwrap();
        // 查询安装进度为NotConfigured且os不为空的主机
        let mut stmt = conn
            .prepare(
                "SELECT ip_address, os FROM hosts WHERE install_progress = ?1 AND os IS NOT NULL",
            )
            .unwrap();
        let host_iter = stmt
            .query_map(params![Progress::NotConfigured as i32], |row| {
                Ok(Host {
                    ip_address: row.get(0)?,
                    os: row.get(1)?,
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
                "SELECT ip_address, os FROM hosts WHERE install_progress = ?1 AND os IS NOT NULL",
            )
            .unwrap();
        stmt.query_map(params![Progress::RebootingToKickstart as i32], |row| {
            Ok(Host {
                ip_address: row.get(0)?,
                os: row.get(1)?,
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
            run_ssh_command_on_host(&host.ip_address, "/sbin/reboot").await;
            println!("[INFO] Rebooting host: {}", host.ip_address);
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
        // 如果当前时间与上次检查时间间隔小于指定的间隔，则等待剩余时间
        let elapsed_time = Utc::now().signed_duration_since(start_time).num_seconds();
        if elapsed_time < interval_secs as i64 {
            tokio::time::sleep(Duration::from_secs(interval_secs - elapsed_time as u64)).await;
        }
    }
}
