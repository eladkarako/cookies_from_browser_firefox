use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use clap::{ArgAction, Parser};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::task::JoinHandle;
#[derive(Parser, Debug)]
#[command(author, version, about = env!("CARGO_PKG_DESCRIPTION"), after_help = "\
Exit codes:
  0  Success
  1  Reserved for internal errors and uncaught exceptions
  2  Invalid command-line syntax or invalid filter argument
  3  Invalid --exclude-expired date or epoch value
  4  Input file is missing, inaccessible, or not a regular file
  5  Malformed or unsupported Netscape input
  6  SQLite database, schema, or query error
  7  Output, write, or flush error
  8  Worker-task failure
  9  Invalid internal state

All dates and epochs are interpreted as UTC.

Date examples:
  date2026                  -> 2026-01-01T00:00:00Z
  date202609                -> 2026-09-01T00:00:00Z
  date20260925              -> 2026-09-25T00:00:00Z
  date2026092513            -> 2026-09-25T13:00:00Z
  date202609251314          -> 2026-09-25T13:14:00Z
  date20260925131400        -> 2026-09-25T13:14:00Z

--exclude-expired without a value, or with the value 'expired', uses
the current UTC time.

Session cookies with expiry 0 are treated like ordinary expiration
timestamps and are excluded when --exclude-expired is used.

Include filters are combined with OR. Exclude filters are combined
with OR. Empty text filter values are ignored.")]
struct Args {
    #[arg(long)]
    path: PathBuf,
    #[arg(long = "include-host", value_name = "TEXT")]
    include_hosts: Vec<String>,
    #[arg(long = "exclude-host", value_name = "TEXT")]
    exclude_hosts: Vec<String>,
    #[arg(long = "include-path", value_name = "TEXT")]
    include_paths: Vec<String>,
    #[arg(long = "exclude-path", value_name = "TEXT")]
    exclude_paths: Vec<String>,
    #[arg(long = "include-cookie-name", value_name = "TEXT")]
    include_cookie_names: Vec<String>,
    #[arg(long = "exclude-cookie-name", value_name = "TEXT")]
    exclude_cookie_names: Vec<String>,
    #[arg(long = "include-cookie-value", value_name = "TEXT")]
    include_cookie_values: Vec<String>,
    #[arg(long = "exclude-cookie-value", value_name = "TEXT")]
    exclude_cookie_values: Vec<String>,
    #[arg(long="include-is-subdomain",action=ArgAction::SetTrue)]
    include_is_subdomain: bool,
    #[arg(long="exclude-is-subdomain",action=ArgAction::SetTrue)]
    exclude_is_subdomain: bool,
    #[arg(long="include-is-secured",action=ArgAction::SetTrue)]
    include_is_secured: bool,
    #[arg(long="exclude-is-secured",action=ArgAction::SetTrue)]
    exclude_is_secured: bool,
    #[arg(long="exclude-expired",value_name="REFERENCE",num_args=0..=1,action=ArgAction::Append)]
    exclude_expired: Vec<Option<String>>,
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
#[derive(Debug, Clone, Copy)]
struct ExpirationReference {
    epoch_seconds: i64,
}
#[derive(Debug, Clone, Copy)]
enum ExpirationParseError {
    InvalidFormat,
    InvalidDate,
    InvalidEpoch,
}
impl std::fmt::Display for ExpirationParseError {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::InvalidFormat => write!(
                formatter,
                "expected 'expired', 'date' followed by digits, or \
                 'epoch' followed by digits"
            ),
            Self::InvalidDate => write!(
                formatter,
                "invalid UTC date; accepted value lengths are 4, 6, 8, \
                 10, 12, or 14 digits: YYYY, YYYYMM, YYYYMMDD, \
                 YYYYMMDDHH, YYYYMMDDHHMM, YYYYMMDDHHMMSS"
            ),
            Self::InvalidEpoch => {
                write!(formatter, "invalid epoch value")
            }
        }
    }
}
#[derive(Debug)]
struct NormalizedFilters {
    include_hosts: Vec<String>,
    exclude_hosts: Vec<String>,
    include_paths: Vec<String>,
    exclude_paths: Vec<String>,
    include_cookie_names: Vec<String>,
    exclude_cookie_names: Vec<String>,
    include_cookie_values: Vec<String>,
    exclude_cookie_values: Vec<String>,
}
impl NormalizedFilters {
    fn from_args(args: &Args) -> Self {
        Self {
            include_hosts: normalize_values(&args.include_hosts),
            exclude_hosts: normalize_values(&args.exclude_hosts),
            include_paths: normalize_values(&args.include_paths),
            exclude_paths: normalize_values(&args.exclude_paths),
            include_cookie_names: normalize_values(
                &args.include_cookie_names,
            ),
            exclude_cookie_names: normalize_values(
                &args.exclude_cookie_names,
            ),
            include_cookie_values: normalize_values(
                &args.include_cookie_values,
            ),
            exclude_cookie_values: normalize_values(
                &args.exclude_cookie_values,
            ),
        }
    }
}
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    if !args.path.is_file() {
        eprintln!(
            "Error: input path is missing or is not a regular file: {}",
            args.path.display()
        );
        eprintln!("Exit code: 4");
        std::process::exit(4);
    }
    let filters = NormalizedFilters::from_args(&args);
    print_arguments(&args, &filters);
    let input_type = detect_input_type(&args.path)?;
    let mut cookies = match input_type {
        InputType::Sqlite => load_sqlite_cookies(&args.path).await?,
        InputType::Netscape => load_netscape_cookies(&args.path)?,
    };
    eprintln!("Loaded cookies: {}", cookies.len());
    apply_normal_filters(&mut cookies, &args, &filters);
    eprintln!(
        "After all include/exclude filters: {}",
        cookies.len()
    );
    if !args.exclude_expired.is_empty() {
        let references = args.exclude_expired.iter().map(|value| {
            parse_expiration_reference(value.as_deref()).unwrap_or_else(|error| {
                let displayed = value.as_deref().unwrap_or("expired");
                eprintln!("Error: invalid --exclude-expired value: {displayed}");
                eprintln!("Reason: {error}");
                eprintln!("Exit code: 3");
                std::process::exit(3);
            })
        }).collect::<Vec<_>>();
        let selected = references
            .iter()
            .copied()
            .min_by_key(|reference| reference.epoch_seconds)
            .expect("references cannot be empty");
        let (expired_count, session_count) =
            apply_expiration_filter(&mut cookies, selected);
        eprintln!(
            "Expiration filters supplied: {}",
            references.len()
        );
        eprintln!(
            "Selected oldest expiration reference: {} UTC",
            format_utc(selected.epoch_seconds)
        );
        eprintln!("Expired cookies excluded: {expired_count}");
        eprintln!("Session cookies excluded: {session_count}");
        eprintln!("After expiration filter: {}", cookies.len());
    }
    sort_cookies(&mut cookies);
    let output = serialize_netscape(&cookies);
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    writer.write_all(&output).context("failed to write output")?;
    writer.flush().context("failed to flush output")?;
    Ok(())
}
fn print_arguments(
    args: &Args,
    filters: &NormalizedFilters,
) {
    eprintln!("Arguments:");
    eprintln!("  input: {}", args.path.display());
    eprintln!("  include-host: {:?}", filters.include_hosts);
    eprintln!("  include-path: {:?}", filters.include_paths);
    eprintln!(
        "  include-cookie-name: {:?}",
        filters.include_cookie_names
    );
    eprintln!(
        "  include-cookie-value: {:?}",
        filters.include_cookie_values
    );
    eprintln!(
        "  include-is-subdomain: {}",
        args.include_is_subdomain
    );
    eprintln!("  include-is-secured: {}", args.include_is_secured);
    eprintln!("  exclude-host: {:?}", filters.exclude_hosts);
    eprintln!("  exclude-path: {:?}", filters.exclude_paths);
    eprintln!(
        "  exclude-cookie-name: {:?}",
        filters.exclude_cookie_names
    );
    eprintln!(
        "  exclude-cookie-value: {:?}",
        filters.exclude_cookie_values
    );
    eprintln!(
        "  exclude-is-subdomain: {}",
        args.exclude_is_subdomain
    );
    eprintln!("  exclude-is-secured: {}", args.exclude_is_secured);
    eprintln!("  exclude-expired: {:?}", args.exclude_expired);
}
fn normalize_values(values: &[String]) -> Vec<String> {
    let mut values = values
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
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
    let worker_count = available_workers
        .saturating_sub(1)
        .clamp(1, 8)
        .min(row_count);
    let rows_per_worker = row_count.div_ceil(worker_count);
    eprintln!("SQLite rows: {row_count}");
    eprintln!("SQLite workers: {worker_count}");
    let database_path = Arc::new(path.to_path_buf());
    let mut handles: Vec<JoinHandle<Result<Vec<Cookie>>>> =
        Vec::with_capacity(worker_count);
    for worker_index in 0..worker_count {
        let offset = worker_index * rows_per_worker;
        let remaining = row_count.saturating_sub(offset);
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
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
        .with_context(|| format!("failed to open {}", path.display()))
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
        parse_bool_field(fields[1]).with_context(|| { format!("invalid include_subdomains field on line {line_number}") })?;
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
fn apply_normal_filters(
    cookies: &mut Vec<Cookie>,
    args: &Args,
    filters: &NormalizedFilters,
) {
    let has_include_filters = !filters.include_hosts.is_empty()
        || !filters.include_paths.is_empty()
        || !filters.include_cookie_names.is_empty()
        || !filters.include_cookie_values.is_empty()
        || args.include_is_subdomain
        || args.include_is_secured;
    if has_include_filters {
        cookies.retain(|cookie| {
            let host = cookie.host.to_ascii_lowercase();
            let path = cookie.path.to_ascii_lowercase();
            let name = cookie.name.to_ascii_lowercase();
            let value = cookie.value.to_ascii_lowercase();
            filters
                .include_hosts
                .iter()
                .any(|pattern| host.contains(pattern))
                || filters
                .include_paths
                .iter()
                .any(|pattern| path.contains(pattern))
                || filters
                .include_cookie_names
                .iter()
                .any(|pattern| name.contains(pattern))
                || filters
                .include_cookie_values
                .iter()
                .any(|pattern| value.contains(pattern))
                || (args.include_is_subdomain
                && cookie.host.starts_with('.'))
                || (args.include_is_secured && cookie.secure)
        });
    }
    cookies.retain(|cookie| {
        let host = cookie.host.to_ascii_lowercase();
        let path = cookie.path.to_ascii_lowercase();
        let name = cookie.name.to_ascii_lowercase();
        let value = cookie.value.to_ascii_lowercase();
        let excluded = filters
            .exclude_hosts
            .iter()
            .any(|pattern| host.contains(pattern))
            || filters
            .exclude_paths
            .iter()
            .any(|pattern| path.contains(pattern))
            || filters
            .exclude_cookie_names
            .iter()
            .any(|pattern| name.contains(pattern))
            || filters
            .exclude_cookie_values
            .iter()
            .any(|pattern| value.contains(pattern))
            || (args.exclude_is_subdomain
            && cookie.host.starts_with('.'))
            || (args.exclude_is_secured && cookie.secure);
        !excluded
    });
}
fn apply_expiration_filter(
    cookies: &mut Vec<Cookie>,
    reference: ExpirationReference,
) -> (usize, usize) {
    let mut expired_count = 0;
    let mut session_count = 0;
    cookies.retain(|cookie| {
        let expired = cookie.expiry <= reference.epoch_seconds;
        if expired {
            expired_count += 1;
            if cookie.expiry == 0 {
                session_count += 1;
            }
        }
        !expired
    });
    (expired_count, session_count)
}
fn parse_expiration_reference(
    value: Option<&str>
) -> Result<ExpirationReference, ExpirationParseError> {
    let value = value.unwrap_or("expired").trim();
    if value.eq_ignore_ascii_case("expired") {
        return Ok(ExpirationReference {
            epoch_seconds: Utc::now().timestamp(),
        });
    }
    if value.len() >= 4 && value[..4].eq_ignore_ascii_case("date") {
        return parse_date_reference(&value[4..]);
    }
    if value.len() >= 5 && value[..5].eq_ignore_ascii_case("epoch") {
        return parse_epoch_reference(&value[5..]);
    }
    Err(ExpirationParseError::InvalidFormat)
}
fn parse_date_reference(
    value: &str
) -> Result<ExpirationReference, ExpirationParseError> {
    let digits: String = value
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    if !matches!(digits.len(), 4 | 6 | 8 | 10 | 12 | 14) {
        return Err(ExpirationParseError::InvalidDate);
    }
    let year = digits[0..4]
        .parse::<i32>()
        .map_err(|_| ExpirationParseError::InvalidDate)?;
    let month = if digits.len() >= 6 {
        digits[4..6]
            .parse::<u32>()
            .map_err(|_| ExpirationParseError::InvalidDate)?
    } else {
        1
    };
    let day = if digits.len() >= 8 {
        digits[6..8]
            .parse::<u32>()
            .map_err(|_| ExpirationParseError::InvalidDate)?
    } else {
        1
    };
    let hour = if digits.len() >= 10 {
        digits[8..10]
            .parse::<u32>()
            .map_err(|_| ExpirationParseError::InvalidDate)?
    } else {
        0
    };
    let minute = if digits.len() >= 12 {
        digits[10..12]
            .parse::<u32>()
            .map_err(|_| ExpirationParseError::InvalidDate)?
    } else {
        0
    };
    let second = if digits.len() >= 14 {
        digits[12..14]
            .parse::<u32>()
            .map_err(|_| ExpirationParseError::InvalidDate)?
    } else {
        0
    };
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or(ExpirationParseError::InvalidDate)?;
    let datetime = date
        .and_hms_opt(hour, minute, second)
        .ok_or(ExpirationParseError::InvalidDate)?;
    Ok(ExpirationReference {
        epoch_seconds: datetime.and_utc().timestamp(),
    })
}
fn parse_epoch_reference(
    value: &str
) -> Result<ExpirationReference, ExpirationParseError> {
    let digits: String = value
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return Err(ExpirationParseError::InvalidEpoch);
    }
    let normalized = if digits.len() < 10 {
        format!("{digits:0>10}")
    } else {
        digits[..10].to_string()
    };
    let epoch_seconds = normalized
        .parse::<i64>()
        .map_err(|_| ExpirationParseError::InvalidEpoch)?;
    Ok(ExpirationReference { epoch_seconds })
}
fn format_utc(epoch_seconds: i64) -> String {
    DateTime::<Utc>::from_timestamp(epoch_seconds, 0)
        .map(|datetime| {
            datetime.format("%Y-%m-%dT%H:%M:%SZ").to_string()
        })
        .unwrap_or_else(|| "invalid UTC timestamp".to_string())
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
fn serialize_netscape(cookies: &[Cookie]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(b"# Netscape HTTP Cookie File\n");
    output.extend_from_slice(
        format!(
            "# Generated by {} v{}\n",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        )
            .as_bytes(),
    );
    output.extend_from_slice(b"# Fields: domain<TAB>include_subdomains<TAB>path<TAB>secure<TAB>\
expiration<TAB>name<TAB>value\n", );
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
