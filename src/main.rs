//C:\Users\Elad\RustroverProjects\cookies_from_browser_firefox\src\main-cd53687b.002.rs
use anyhow::{Context, Result};
use clap::Parser;
use rusqlite::{Connection, OpenFlags};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::task::JoinHandle;
#[derive(Parser, Debug)]
#[command(author, version, about = env!("CARGO_PKG_DESCRIPTION"), )]
struct Args {
    #[arg(long)]
    path: PathBuf,
    #[arg(long = "include", value_name = "TEXT")]
    includes: Vec<String>,
    #[arg(long = "exclude", value_name = "TEXT")]
    excludes: Vec<String>,
}
#[derive(Debug, Clone)]
struct Cookie {
    host: String,
    path: String,
    secure: bool,
    http_only: bool,
    expiry: i64,
    name: String,
    value: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputType {
    Sqlite,
    Netscape,
}
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    if !args.path.is_file() {
        anyhow::bail!(
            "input file does not exist: {}",
            args.path.display()
        );
    }
    let input_type = detect_input_type(&args.path)?;
    let mut cookies = match input_type {
        InputType::Sqlite => load_sqlite_cookies(&args.path).await?,
        InputType::Netscape => load_netscape_cookies(&args.path)?,
    };
    eprintln!("Loaded cookies: {}", cookies.len());
    sort_cookies(&mut cookies);
    filter_cookies(&mut cookies, &args.includes, &args.excludes);
    eprintln!("Output cookies: {}", cookies.len());
    let output = serialize_netscape(&cookies);
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    writer.write_all(&output).context("failed to write output")?;
    writer.flush().context("failed to flush output")?;
    Ok(())
}
fn detect_input_type(path: &Path) -> Result<InputType> {
    let mut file = File::open(path).with_context(|| {
        format!("failed to open {}", path.display())
    })?;
    let mut header = [0u8; 16];
    let bytes_read = file.read(&mut header).with_context(|| {
        format!("failed to read {}", path.display())
    })?;
    if bytes_read == 16 && &header == b"SQLite format 3\0" {
        Ok(InputType::Sqlite)
    } else {
        Ok(InputType::Netscape)
    }
}
async fn load_sqlite_cookies(path: &Path) -> Result<Vec<Cookie>> {
    let row_count = {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || count_rows(&path))
            .await
            .context("row-count task panicked")??
    };
    if row_count == 0 {
        return Ok(Vec::new());
    }
    let available_workers = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);
    let worker_count = available_workers.min(row_count);
    let rows_per_worker = row_count.div_ceil(worker_count);
    eprintln!("SQLite rows: {row_count}");
    eprintln!("SQLite workers: {worker_count}");
    let database_path = Arc::new(path.to_path_buf());
    let mut handles: Vec<JoinHandle<Result<Vec<Cookie>>>> =
        Vec::with_capacity(worker_count);
    for worker_index in 0..worker_count {
        let offset = worker_index * rows_per_worker;
        let remaining = row_count - offset;
        let limit = remaining.min(rows_per_worker);
        if limit == 0 {
            continue;
        }
        let database_path = Arc::clone(&database_path);
        let handle = tokio::task::spawn_blocking(move || {
            read_sqlite_chunk(&database_path, offset, limit)
        });
        handles.push(handle);
    }
    let mut cookies = Vec::with_capacity(row_count);
    for handle in handles {
        let worker_cookies = handle
            .await
            .context("SQLite reader task panicked")??;
        cookies.extend(worker_cookies);
    }
    Ok(cookies)
}
fn open_readonly(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY;
    Connection::open_with_flags(path, flags).with_context(|| {
        format!("failed to open {}", path.display())
    })
}
fn count_rows(path: &Path) -> Result<usize> {
    let connection = open_readonly(path)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM moz_cookies",
        [],
        |row| row.get(0),
    )?;
    usize::try_from(count)
        .context("SQLite row count does not fit in usize")
}
fn read_sqlite_chunk(
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<Vec<Cookie>> {
    let connection = open_readonly(path)?;
    let mut statement = connection.prepare(
        r#"
        SELECT
            host,
            path,
            isSecure,
            isHttpOnly,
            expiry,
            name,
            value
        FROM moz_cookies
        LIMIT ?1 OFFSET ?2
        "#,
    )?;
    let limit_i64 = i64::try_from(limit)
        .context("chunk size does not fit in SQLite integer")?;
    let offset_i64 = i64::try_from(offset)
        .context("offset does not fit in SQLite integer")?;
    let mut rows = statement.query((limit_i64, offset_i64))?;
    let mut cookies = Vec::with_capacity(limit);
    while let Some(row) = rows.next()? {
        let host =
            row.get::<_, Option<String>>(0)?.unwrap_or_default();
        let path = row
            .get::<_, Option<String>>(1)?
            .unwrap_or_else(|| "/".to_string());
        let is_secure = row.get::<_, Option<i64>>(2)?.unwrap_or(0);
        let is_http_only =
            row.get::<_, Option<i64>>(3)?.unwrap_or(0);
        let expiry = row.get::<_, Option<i64>>(4)?.unwrap_or(0);
        let name =
            row.get::<_, Option<String>>(5)?.unwrap_or_default();
        let value =
            row.get::<_, Option<String>>(6)?.unwrap_or_default();
        cookies.push(Cookie {
            host,
            path,
            secure: is_secure != 0,
            http_only: is_http_only != 0,
            expiry,
            name,
            value,
        });
    }
    Ok(cookies)
}
fn load_netscape_cookies(path: &Path) -> Result<Vec<Cookie>> {
    let file = File::open(path).with_context(|| {
        format!("failed to open {}", path.display())
    })?;
    let reader = BufReader::new(file);
    let mut cookies = Vec::new();
    for (line_index, line_result) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = line_result.with_context(|| {
            format!("failed reading line {line_number}")
        })?;
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') && !line.starts_with("#HttpOnly_") {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 7 {
            anyhow::bail!(
                "invalid Netscape cookie line {line_number}: \
                 expected 7 tab-separated fields, found {}",
                fields.len()
            );
        }
        let (http_only, host) = if let Some(host) =
            fields[0].strip_prefix("#HttpOnly_")
        {
            (true, host.to_string())
        } else {
            (false, fields[0].to_string())
        };
        let _include_subdomains = parse_bool_field(fields[1]).with_context(|| { format!("invalid include_subdomains field on line {line_number}") })?;
        let secure =
            parse_bool_field(fields[3]).with_context(|| {
                format!("invalid secure field on line {line_number}")
            })?;
        let expiry =
            fields[4].parse::<i64>().with_context(|| {
                format!(
                    "invalid expiration field on line {line_number}"
                )
            })?;
        cookies.push(Cookie {
            host,
            path: fields[2].to_string(),
            secure,
            http_only,
            expiry,
            name: fields[5].to_string(),
            value: fields[6].to_string(),
        });
    }
    Ok(cookies)
}
fn parse_bool_field(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => anyhow::bail!("expected TRUE/FALSE, got {other:?}"),
    }
}
fn sort_cookies(cookies: &mut [Cookie]) {
    cookies.sort_unstable_by(|a, b| {
        let a_host = a.host.trim_start_matches('.');
        let b_host = b.host.trim_start_matches('.');
        a_host
            .cmp(b_host)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.host.cmp(&b.host))
            .then_with(|| a.value.cmp(&b.value))
            .then_with(|| a.expiry.cmp(&b.expiry))
            .then_with(|| a.secure.cmp(&b.secure))
            .then_with(|| a.http_only.cmp(&b.http_only))
    });
}
fn filter_cookies(
    cookies: &mut Vec<Cookie>,
    includes: &[String],
    excludes: &[String],
) {
    let includes: Vec<String> =
        includes.iter().map(|value| value.to_lowercase()).collect();
    let excludes: Vec<String> =
        excludes.iter().map(|value| value.to_lowercase()).collect();
    cookies.retain(|cookie| {
        let host = cookie.host.to_lowercase();
        let excluded =
            excludes.iter().any(|pattern| host.contains(pattern));
        if excluded {
            return false;
        }
        if includes.is_empty() {
            return true;
        }
        includes.iter().any(|pattern| host.contains(pattern))
    });
}
fn serialize_netscape(cookies: &[Cookie]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(b"# Netscape HTTP Cookie File\n");
    output.extend_from_slice(
        format!(
            "# Generated by {} v{}\n",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        )
            .as_bytes(),
    );
    output.extend_from_slice(b"# Fields: domain<TAB>include_subdomains<TAB>path<TAB>secure<TAB>expiration<TAB>name<TAB>value\n");
    output.extend_from_slice(
        b"# expiration is a Unix timestamp in seconds\n",
    );
    output.extend_from_slice(b"# include_subdomains is TRUE when the domain begins with a dot\n");
    output.extend_from_slice(b"#\n");
    for cookie in cookies {
        let domain = if cookie.http_only {
            format!("#HttpOnly_{}", cookie.host)
        } else {
            cookie.host.clone()
        };
        append_field(&mut output, &domain);
        output.push(b'\t');
        append_field(
            &mut output,
            if cookie.host.starts_with('.') {
                "TRUE"
            } else {
                "FALSE"
            },
        );
        output.push(b'\t');
        append_field(&mut output, &cookie.path);
        output.push(b'\t');
        append_field(
            &mut output,
            if cookie.secure { "TRUE" } else { "FALSE" },
        );
        output.push(b'\t');
        append_field(&mut output, &cookie.expiry.to_string());
        output.push(b'\t');
        append_field(&mut output, &cookie.name);
        output.push(b'\t');
        append_field(&mut output, &cookie.value);
        output.push(b'\n');
    }
    output
}
fn append_field(
    output: &mut Vec<u8>,
    value: &str,
) {
    for byte in value.bytes() {
        match byte {
            b'\t' => output.extend_from_slice(b"%09"),
            b'\n' => output.extend_from_slice(b"%0A"),
            b'\r' => output.extend_from_slice(b"%0D"),
            _ => output.push(byte),
        }
    }
}
