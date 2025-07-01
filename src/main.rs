use actix_web::{App, HttpResponse, HttpServer, Responder, web};
use chrono::{NaiveDateTime, Utc};
use rusqlite::{Connection, params};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use tokio::task;
use tokio::time::{self, Duration}; // Add this line

// Shared database connection
type DbPool = Arc<Mutex<Connection>>;

// Initialize database schema
fn init_db(conn: &Connection) {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS hosts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            serial TEXT UNIQUE,
            ip_address TEXT,
            os TEXT,
            install_progress TEXT,
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
        "CREATE TABLE IF NOT EXISTS jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            serial TEXT,
            os TEXT,
            processed INTEGER DEFAULT 0
        )",
        [],
    )
    .unwrap();
}

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

async fn monitor_dhcp_leases(
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

// Handler for `/api/ipxe/{serial}`
async fn get_ipxe_script(serial: web::Path<String>, db_pool: web::Data<DbPool>) -> impl Responder {
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
        let script: Option<String> = conn
            .query_row(
                "SELECT script FROM ipxe WHERE os = ?1",
                params![os],
                |row| row.get(0),
            )
            .ok();
        if let Some(script) = script {
            return HttpResponse::Ok().body(script);
        }
    }
    HttpResponse::NotFound().body("iPXE script not found")
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
    // Define the database file path in the current directory
    let db_path = Path::new("./cloudboot-lce.db");
    // Open or create the SQLite database
    let conn = Connection::open(db_path).unwrap();
    // Initialize the database schema if it doesn't exist
    init_db(&conn);
    let db_pool = Arc::new(Mutex::new(conn));
    // Spawn background task
    let db_pool_clone = db_pool.clone();
    task::spawn(async move {
        process_jobs(db_pool_clone).await;
    });
    let db_pool_clone = db_pool.clone();
    tokio::spawn(async move {
        monitor_dhcp_leases("/var/lib/dhcpd/dhcpd.leases", 10, db_pool_clone, "abc123").await;
    });
    // Start web server
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(db_pool.clone()))
            .route("/api/ipxe/{serial}", web::get().to(get_ipxe_script))
    })
    .bind("127.0.0.1:8000")?
    .run()
    .await
}
