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
pub mod progress_control;

use actix_web::{App, HttpResponse, HttpServer, Responder, web};
use rusqlite::{Connection, params};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::task;
use tokio::time::{self, Duration};

use crate::database_init::init_db;
use crate::hosts_discovery::monitor_dhcp_leases;
use crate::progress_control::progress_control;

// 数据库地址
const DB_PATH: &str = "./cloudboot-lce.db";

// 数据库连接池互斥锁
type DbPool = Arc<Mutex<Connection>>;

// Handler for `/api/ipxe/{serial}`
async fn get_ipxe_script(serial: web::Path<String>, db_pool: web::Data<DbPool>) -> impl Responder {
    println!("[INFO] Offering iPXE script for {serial}");
    let conn = db_pool.lock().unwrap();
    let serial = serial.into_inner();
    let os: Option<String> = conn
        .query_row(
            "SELECT os FROM hosts WHERE serial = ?1",
            params![serial],
            |row| row.get(0),
        )
        .ok();
    if let Some(os) = os {
        let script_path: Option<String> = conn
            .query_row(
                "SELECT script FROM ipxe WHERE os = ?1",
                params![os],
                |row| row.get(0),
            )
            .ok();
        if let Some(path) = script_path {
            match fs::read_to_string(&path) {
                Ok(script) => return HttpResponse::Ok().body(script),
                Err(_) => {
                    return HttpResponse::InternalServerError()
                        .body(format!("Error reading script file {}", path));
                }
            }
        }
    }
    println!("[INFO] No iPXE script found for serial {serial}");
    HttpResponse::NotFound().body("")
}

// Background task to process jobs
async fn process_jobs(db_pool: DbPool) {
    let mut interval = time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        let conn = db_pool.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, serial, os FROM jobs WHERE processed = 0")
            .unwrap();
        let jobs = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap();
        for job in jobs {
            if let Ok((id, serial, os)) = job {
                conn.execute(
                    "INSERT OR REPLACE INTO hosts (serial, os) VALUES (?1, ?2)",
                    params![serial, os],
                )
                .unwrap();
                conn.execute("UPDATE jobs SET processed = 1 WHERE id = ?1", params![id])
                    .unwrap();
            }
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // 初始化数据库
    let db_path = Path::new(DB_PATH);
    let conn = Connection::open(db_path).unwrap();
    init_db(&conn);
    // 监控 dhcp.leases
    let db_pool = Arc::new(Mutex::new(conn));
    let db_pool_clone = db_pool.clone();
    tokio::spawn(async move {
        monitor_dhcp_leases("/var/lib/dhcpd/dhcpd.leases", 10, db_pool_clone).await;
    });
    // 处理装机任务
    // let db_pool_clone = db_pool.clone();
    // task::spawn(async move {
    //     process_jobs(db_pool_clone).await;
    // });
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
