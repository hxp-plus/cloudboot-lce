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

use rusqlite::Connection;

// 初始化数据库
pub fn init_db(conn: &Connection) {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS hosts (
            serial TEXT UNIQUE,
            ip_address TEXT,
            ipmi_address TEXT PRIMARY KEY,
            os TEXT,
            hostname TEXT,
            public_ip_addr TEXT,
            vlan_id INTEGER,
            install_progress INTEGER,
            last_updated TEXT
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ipxe (
            os TEXT PRIMARY KEY,
            script TEXT
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS install_queue (
            ipmi_address TEXT PRIMARY KEY
        )",
        [],
    )
    .unwrap();
}
