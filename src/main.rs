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

pub mod command_execute;
pub mod database_init;
pub mod hosts_discovery;
pub mod ipxe_script;
pub mod progress_control;

use actix_web::{App, HttpServer, web};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use tokio::task;

use crate::database_init::init_db;
use crate::hosts_discovery::monitor_dhcp_leases;
use crate::ipxe_script::get_ipxe_script;
use crate::progress_control::progress_control;

// 数据库地址
const DB_PATH: &str = "./cloudboot-lce.db";
const SQL_CONN_POOL_SIZE: u32 = 8;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // 初始化连接池 (新增)
    let manager = SqliteConnectionManager::file(DB_PATH);
    let db_pool = Pool::builder()
        .max_size(SQL_CONN_POOL_SIZE) // 根据需求调整连接数
        .build(manager)
        .unwrap();
    // 初始化数据库
    let conn = db_pool.get().unwrap();
    init_db(&conn);
    // 监控 dhcp.leases
    let db_pool_clone = db_pool.clone();
    tokio::spawn(async move {
        monitor_dhcp_leases("/var/lib/dhcpd/dhcpd.leases", 10, db_pool_clone).await;
    });
    // 进行装机进度控制
    let db_pool_clone = db_pool.clone();
    task::spawn(async move {
        progress_control(10, db_pool_clone).await;
    });
    // 受理 HTTP 请求
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(db_pool.clone()))
            .route("/api/ipxe/{serial}", web::get().to(get_ipxe_script))
    })
    .bind("127.0.0.1:8000")?
    .run()
    .await
}
