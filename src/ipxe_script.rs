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

// iPXE 脚本生成代码
use actix_web::{HttpResponse, Responder, web};
use rusqlite::{Connection, params};
use std::fs;
use std::sync::{Arc, Mutex};

use crate::progress_control::progress;

// 数据库连接池互斥锁
type DbPool = Arc<Mutex<Connection>>;

// 处理 /api/ipxe/{serial}
pub async fn get_ipxe_script(
    serial: web::Path<String>,
    db_pool: web::Data<DbPool>,
) -> impl Responder {
    println!("[INFO] Offering iPXE script for {serial}");
    let conn = db_pool.lock().unwrap();
    let serial = serial.into_inner();
    let os: Option<String> = conn
        .query_row(
            "SELECT os FROM hosts WHERE serial = ?1 and install_progress = ?2",
            params![serial, progress::RebootingToKickstart as i32],
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
                Ok(script) => {
                    println!("[INFO] iPXE script for {serial}: {path}");
                    return HttpResponse::Ok().body(script);
                }
                Err(_) => {
                    println!("[ERROR] Error reading script file: {path}");
                    return HttpResponse::InternalServerError().body("");
                }
            }
        }
    }
    println!("[INFO] No iPXE script found for serial {serial}");
    HttpResponse::NotFound().body("")
}
