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
    os: String,
    install_progress: i32,
}

enum progress {
    NotConfigured = 0,
    RebootingToKickstart = 5,
    KickstartLoaded = 10,
    PreInstallFinished = 20,
    PostInstallFinished = 60,
    InstallFinished = 100,
}

// 将所有尚未开始安装但是配置了操作系统的机器，安装进度置为正在重启到kickstart
fn start_kickstart_installation(db_pool: Arc<Mutex<Connection>>) {
    let conn = db_pool.lock().unwrap();
    // 查询安装进度为NotConfigured且os不为空的主机
    let mut stmt = conn
        .prepare("SELECT ip_address, os, install_progress FROM hosts WHERE install_progress = ?1 AND os IS NOT NULL")
        .unwrap();
    let host_iter = stmt
        .query_map(params![progress::NotConfigured as i32], |row| {
            Ok(Host {
                ip_address: row.get(0)?,
                os: row.get(1)?,
                install_progress: row.get(2)?,
            })
        })
        .unwrap();
    let hosts: Vec<Host> = host_iter.filter_map(Result::ok).collect();
    // 检查每个主机的os是否在ipxe表且ipxe表里script不为空
    for host in hosts {
        let mut ipxe_stmt = conn
            .prepare("SELECT script FROM ipxe WHERE os = ?1 AND script IS NOT NULL")
            .unwrap();
        let script_iter = ipxe_stmt
            .query_map(params![host.os], |row| row.get::<_, String>(0))
            .unwrap();
        // 如果ipxe不为空，且主机为未配置状态，将主机上的安装进度设置为 RebootingToKickstart
        if script_iter.count() > 0 && host.install_progress == progress::NotConfigured as i32 {
            run_ssh_command_on_host(
                &host.ip_address,
                &format!(
                    "echo \"{}\" >/tmp/install-progress",
                    progress::RebootingToKickstart as i32
                ),
            );
            println!(
                "[INFO] Setting host {} install progress to: RebootingToKickstart",
                host.ip_address
            );
        }
    }
}

// 持续监控主机状态，并在达到进度时下发操作
pub async fn progress_control(interval_secs: u64, db_pool: Arc<Mutex<Connection>>) {
    let mut interval = time::interval(Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        // 将所有满足装机条件的机器状态设置为RebootingToKickstart
        start_kickstart_installation(db_pool.clone());
        // 重启所有状态为RebootingToKickstart的机器
    }
}
