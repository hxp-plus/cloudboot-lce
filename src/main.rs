mod modules;
use modules::hosts_discovery::monitor_dhcp_leases;

use actix_web::{App, HttpResponse, HttpServer, Responder, web};
use rusqlite::{Connection, params};
use std::path::Path;
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
