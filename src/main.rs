//! Load, sort, filter, and output Firefox or Netscape cookie files.
//!
//! The input can be either:
//!
//! - A Firefox `cookies.sqlite` database.
//! - A Netscape-format cookie text file.
//!
//! The input type is detected from the first 16 bytes. SQLite databases
//! begin with `SQLite format 3\0`; all other files are treated as Netscape
//! cookie files.
//!
//! Output is always written in Netscape cookie-file format.

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

/// Command-line arguments accepted by the program.
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = env!("CARGO_PKG_DESCRIPTION"),
)]
struct Args {
    /// Input path.
    ///
    /// This can be a Firefox cookies.sqlite database or a Netscape
    /// cookie text file.
    #[arg(long)]
    path: PathBuf,

    /// Include hosts containing this case-insensitive partial match.
    ///
    /// This option may be specified multiple times. Multiple include
    /// values use OR semantics.
    ///
    /// For example:
    ///
    ///     --include google --include mozilla
    ///
    /// keeps a cookie if its host contains either "google" or "mozilla".
    #[arg(long = "include", value_name = "TEXT")]
    includes: Vec<String>,

    /// Exclude hosts containing this case-insensitive partial match.
    ///
    /// This option may be specified multiple times. Exclude filters take
    /// precedence over include filters.
    ///
    /// For example:
    ///
    ///     --exclude ads --exclude tracker
    ///
    /// removes a cookie if its host contains either "ads" or "tracker".
    #[arg(long = "exclude", value_name = "TEXT")]
    excludes: Vec<String>,
}

/// One cookie represented independently of its input format.
///
/// The Netscape file format stores `HttpOnly` as part of the domain field
/// using the `#HttpOnly_` prefix. Internally, this prefix is removed and
/// represented by the separate `http_only` field.
#[derive(Debug, Clone)]
struct Cookie {
    /// Cookie domain, without the Netscape `#HttpOnly_` prefix.
    host: String,

    /// Cookie path.
    path: String,

    /// Whether the cookie is restricted to HTTPS.
    secure: bool,

    /// Whether JavaScript cannot access the cookie.
    http_only: bool,

    /// Expiration time as a Unix timestamp in seconds.
    expiry: i64,

    /// Cookie name.
    name: String,

    /// Cookie value.
    value: String,
}

/// The supported input file types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputType {
    /// A Firefox SQLite cookie database.
    Sqlite,

    /// A Netscape-format cookie text file.
    Netscape,
}

/// Program entry point.
///
/// The function loads the input, sorts it, applies filters, serializes the
/// final result into an in-memory byte buffer, and writes that buffer to
/// standard output.
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    // Parse command-line arguments.
    let args = Args::parse();

    // Check that the input path exists and is a regular file.
    if !args.path.is_file() {
        anyhow::bail!(
            "input file does not exist: {}",
            args.path.display()
        );
    }

    // Detect whether the input is SQLite or Netscape text.
    let input_type = detect_input_type(&args.path)?;

    // Load all cookies into one common in-memory representation.
    let mut cookies = match input_type {
        // SQLite reading uses multiple blocking worker tasks.
        InputType::Sqlite => load_sqlite_cookies(&args.path).await?,

        // Netscape input is already text and does not require SQLite.
        InputType::Netscape => load_netscape_cookies(&args.path)?,
    };

    // Report the number of records loaded before filtering.
    eprintln!("Loaded cookies: {}", cookies.len());

    // Sort before filtering, as requested.
    sort_cookies(&mut cookies);

    // Apply include and exclude filters to the sorted records.
    filter_cookies(&mut cookies, &args.includes, &args.excludes);

    // Report the number of records remaining after filtering.
    eprintln!("Output cookies: {}", cookies.len());

    // Serialize all records into one cached output byte buffer.
    let output = serialize_netscape(&cookies);

    // Buffer the pipe output to reduce the number of write operations.
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    // Write the complete cached output to stdout.
    writer.write_all(&output).context("failed to write output")?;

    // Flush the buffered writer so all bytes reach the pipe.
    writer.flush().context("failed to flush output")?;

    Ok(())
}

/// Detects whether `path` contains SQLite or Netscape-format data.
///
/// SQLite databases have a fixed 16-byte header:
///
/// ```text
/// SQLite format 3\0
/// ```
///
/// Any file that does not have this header is treated as a Netscape cookie
/// file.
fn detect_input_type(path: &Path) -> Result<InputType> {
    // Open the file for reading its header.
    let mut file = File::open(path).with_context(|| {
        format!("failed to open {}", path.display())
    })?;

    // The SQLite signature is exactly 16 bytes long.
    let mut header = [0u8; 16];

    // Read up to 16 bytes from the beginning of the file.
    let bytes_read = file.read(&mut header).with_context(|| {
        format!("failed to read {}", path.display())
    })?;

    // Compare the complete SQLite signature.
    if bytes_read == 16 && &header == b"SQLite format 3\0" {
        Ok(InputType::Sqlite)
    } else {
        // Non-SQLite input is interpreted as Netscape text.
        Ok(InputType::Netscape)
    }
}

/// Loads cookies from a Firefox SQLite database.
///
/// The database is divided into chunks and read by blocking worker tasks.
/// Every worker returns structured `Cookie` values. The calling task waits
/// for every worker and combines their results before sorting.
async fn load_sqlite_cookies(path: &Path) -> Result<Vec<Cookie>> {
    // Count the rows before dividing the database into worker chunks.
    let row_count = {
        // Move an owned path into the blocking task.
        let path = path.to_path_buf();

        // SQLite operations are blocking, so run the count outside the
        // asynchronous executor thread.
        tokio::task::spawn_blocking(move || count_rows(&path))
            .await
            .context("row-count task panicked")??
    };

    // Return an empty result for an empty database.
    if row_count == 0 {
        return Ok(Vec::new());
    }

    // Determine how many blocking workers can reasonably be used.
    let available_workers = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);

    // Do not create more workers than rows.
    let worker_count = available_workers.min(row_count);

    // Divide the rows approximately evenly between workers.
    let rows_per_worker = row_count.div_ceil(worker_count);

    // Report SQLite processing details on stderr.
    eprintln!("SQLite rows: {row_count}");
    eprintln!("SQLite workers: {worker_count}");

    // Share the database path between worker tasks.
    let database_path = Arc::new(path.to_path_buf());

    // Each worker returns a vector of structured cookies.
    let mut handles: Vec<JoinHandle<Result<Vec<Cookie>>>> =
        Vec::with_capacity(worker_count);

    // Create one task for each database chunk.
    for worker_index in 0..worker_count {
        // Calculate the first row assigned to this worker.
        let offset = worker_index * rows_per_worker;

        // Calculate how many rows remain after this worker's offset.
        let remaining = row_count - offset;

        // Limit this worker to its normal chunk size or the remaining rows.
        let limit = remaining.min(rows_per_worker);

        // Avoid starting an empty worker.
        if limit == 0 {
            continue;
        }

        // Clone the shared path reference for this worker.
        let database_path = Arc::clone(&database_path);

        // Spawn the blocking SQLite operation.
        let handle = tokio::task::spawn_blocking(move || {
            read_sqlite_chunk(&database_path, offset, limit)
        });

        // Store the handle so it can be joined below.
        handles.push(handle);
    }

    // Allocate enough room for the complete result.
    let mut cookies = Vec::with_capacity(row_count);

    // Wait for every worker and merge all worker results.
    for handle in handles {
        // The first `?` handles task panics; the second handles the
        // Result returned by the worker itself.
        let worker_cookies = handle
            .await
            .context("SQLite reader task panicked")??;

        // Append this worker's records to the combined result.
        cookies.extend(worker_cookies);
    }

    // Sorting is intentionally performed after all workers are joined.
    Ok(cookies)
}

/// Opens a SQLite database in read-only mode.
fn open_readonly(path: &Path) -> Result<Connection> {
    // Use SQLite's read-only open flag.
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY;

    // Open the database and add its path to any error message.
    Connection::open_with_flags(path, flags).with_context(|| {
        format!("failed to open {}", path.display())
    })
}

/// Counts records in the Firefox `moz_cookies` table.
fn count_rows(path: &Path) -> Result<usize> {
    // Open the database without allowing writes.
    let connection = open_readonly(path)?;

    // Query the number of cookie rows.
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM moz_cookies",
        [],
        |row| row.get(0),
    )?;

    // Convert SQLite's signed integer to Rust's platform-sized integer.
    usize::try_from(count)
        .context("SQLite row count does not fit in usize")
}

/// Reads one SQLite chunk and converts it into structured cookies.
fn read_sqlite_chunk(
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<Vec<Cookie>> {
    // Open one independent read-only connection for this worker.
    let connection = open_readonly(path)?;

    // Select the fields needed for Netscape output.
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

    // SQLite bind parameters use signed 64-bit integers.
    let limit_i64 = i64::try_from(limit)
        .context("chunk size does not fit in SQLite integer")?;

    // Convert the offset to SQLite's integer type.
    let offset_i64 = i64::try_from(offset)
        .context("offset does not fit in SQLite integer")?;

    // Execute the chunk query.
    let mut rows = statement.query((limit_i64, offset_i64))?;

    // Reserve enough capacity for this worker's expected result.
    let mut cookies = Vec::with_capacity(limit);

    // Read every row in this worker's chunk.
    while let Some(row) = rows.next()? {
        // Read the cookie domain.
        let host =
            row.get::<_, Option<String>>(0)?.unwrap_or_default();

        // Use "/" if the database path is NULL.
        let path = row
            .get::<_, Option<String>>(1)?
            .unwrap_or_else(|| "/".to_string());

        // SQLite stores boolean values as integer values.
        let is_secure = row.get::<_, Option<i64>>(2)?.unwrap_or(0);

        // Read the separate HttpOnly flag.
        let is_http_only =
            row.get::<_, Option<i64>>(3)?.unwrap_or(0);

        // Read the expiration timestamp.
        let expiry = row.get::<_, Option<i64>>(4)?.unwrap_or(0);

        // Read the cookie name.
        let name =
            row.get::<_, Option<String>>(5)?.unwrap_or_default();

        // Read the cookie value.
        let value =
            row.get::<_, Option<String>>(6)?.unwrap_or_default();

        // Store Secure and HttpOnly independently.
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

    // Return this worker's structured records.
    Ok(cookies)
}

/// Loads a Netscape-format cookie file.
fn load_netscape_cookies(path: &Path) -> Result<Vec<Cookie>> {
    // Open the text file.
    let file = File::open(path).with_context(|| {
        format!("failed to open {}", path.display())
    })?;

    // Read the file line by line.
    let reader = BufReader::new(file);

    // Store parsed cookies here.
    let mut cookies = Vec::new();

    // Process every input line.
    for (line_index, line_result) in reader.lines().enumerate() {
        // Convert the zero-based index into a human-readable line number.
        let line_number = line_index + 1;

        // Read one line and attach its line number to errors.
        let line = line_result.with_context(|| {
            format!("failed reading line {line_number}")
        })?;

        // Remove a Windows carriage return if present.
        let line = line.trim_end_matches('\r');

        // Ignore empty lines.
        if line.is_empty() {
            continue;
        }

        /*
         * Ordinary comments begin with '#'.
         *
         * HttpOnly cookie records are a special exception because their
         * domain begins with "#HttpOnly_".
         */
        if line.starts_with('#') && !line.starts_with("#HttpOnly_") {
            continue;
        }

        // Netscape cookie records must contain exactly seven fields.
        let fields: Vec<&str> = line.split('\t').collect();

        // Reject malformed records instead of silently corrupting output.
        if fields.len() != 7 {
            anyhow::bail!(
                "invalid Netscape cookie line {line_number}: \
                 expected 7 tab-separated fields, found {}",
                fields.len()
            );
        }

        // Detect and remove the special HttpOnly domain prefix.
        let (http_only, host) = if let Some(host) =
            fields[0].strip_prefix("#HttpOnly_")
        {
            (true, host.to_string())
        } else {
            (false, fields[0].to_string())
        };

        // Validate the include-subdomains field.
        //
        // The output later derives this field from the domain's leading
        // dot, but validating the input catches malformed cookie files.
        let _include_subdomains = parse_bool_field(fields[1])
            .with_context(|| {
                format!(
                    "invalid include_subdomains field on line {line_number}"
                )
            })?;

        // Parse the Secure field.
        let secure =
            parse_bool_field(fields[3]).with_context(|| {
                format!("invalid secure field on line {line_number}")
            })?;

        // Parse the Unix expiration timestamp.
        let expiry =
            fields[4].parse::<i64>().with_context(|| {
                format!(
                    "invalid expiration field on line {line_number}"
                )
            })?;

        // Store the parsed Netscape record.
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

    // Return all parsed records.
    Ok(cookies)
}

/// Parses a Netscape boolean field.
///
/// Accepted values are `TRUE`, `FALSE`, `1`, and `0`, without regard to
/// letter case.
fn parse_bool_field(value: &str) -> Result<bool> {
    // Normalize the field before comparing it.
    match value.trim().to_ascii_lowercase().as_str() {
        // These values represent true.
        "true" | "1" => Ok(true),

        // These values represent false.
        "false" | "0" => Ok(false),

        // Reject unknown values.
        other => anyhow::bail!("expected TRUE/FALSE, got {other:?}"),
    }
}

/// Sorts cookies by normalized host, path, and name.
///
/// A leading dot is ignored while comparing hosts:
///
/// ```text
/// .example.com
/// example.com
/// ```
///
/// are compared using the same normalized host, `example.com`.
fn sort_cookies(cookies: &mut [Cookie]) {
    // Sort records in place without allocating a second vector.
    cookies.sort_unstable_by(|a, b| {
        // Remove leading dots only for comparison.
        let a_host = a.host.trim_start_matches('.');
        let b_host = b.host.trim_start_matches('.');

        // Apply the requested primary ordering.
        a_host
            .cmp(b_host)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.name.cmp(&b.name))
            // Use additional fields as deterministic tie-breakers.
            .then_with(|| a.host.cmp(&b.host))
            .then_with(|| a.value.cmp(&b.value))
            .then_with(|| a.expiry.cmp(&b.expiry))
            .then_with(|| a.secure.cmp(&b.secure))
            .then_with(|| a.http_only.cmp(&b.http_only))
    });
}

/// Applies include and exclude filters to the sorted cookie list.
///
/// Filtering uses only the cookie host.
///
/// The logic is:
///
/// ```text
/// if host matches any exclude:
///     remove
/// else if there are no includes:
///     keep
/// else if host matches any include:
///     keep
/// else:
///     remove
/// ```
///
/// Include and exclude matching is case-insensitive and uses substring
/// matching, including any leading dot in the host or filter.
fn filter_cookies(
    cookies: &mut Vec<Cookie>,
    includes: &[String],
    excludes: &[String],
) {
    // Lowercase all filters once instead of once per cookie.
    let includes: Vec<String> =
        includes.iter().map(|value| value.to_lowercase()).collect();

    // Lowercase all exclusion filters once.
    let excludes: Vec<String> =
        excludes.iter().map(|value| value.to_lowercase()).collect();

    // Retain only cookies that pass the filter rules.
    cookies.retain(|cookie| {
        // Lowercase the host for case-insensitive matching.
        let host = cookie.host.to_lowercase();

        // Any exclude match removes the cookie.
        let excluded =
            excludes.iter().any(|pattern| host.contains(pattern));

        // Exclusion always takes precedence.
        if excluded {
            return false;
        }

        // With no include filters, retain every non-excluded cookie.
        if includes.is_empty() {
            return true;
        }

        // With includes, at least one include must match.
        includes.iter().any(|pattern| host.contains(pattern))
    });
}

/// Serializes cookies into a cached Netscape-format byte buffer.
fn serialize_netscape(cookies: &[Cookie]) -> Vec<u8> {
    // Start with an empty output buffer.
    let mut output = Vec::new();

    // Write explanatory comment lines.
    output.extend_from_slice(b"# Netscape HTTP Cookie File\n");
    output.extend_from_slice(
        format!(
            "# Generated by {} v{}\n",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        )
            .as_bytes(),
    );
    output.extend_from_slice(
        b"# Fields: domain<TAB>include_subdomains<TAB>path<TAB>secure<TAB>expiration<TAB>name<TAB>value\n",
    );
    output.extend_from_slice(
        b"# expiration is a Unix timestamp in seconds\n",
    );
    output.extend_from_slice(
        b"# include_subdomains is TRUE when the domain begins with a dot\n",
    );
    output.extend_from_slice(b"#\n");

    // Serialize each sorted and filtered cookie.
    for cookie in cookies {
        // Netscape represents HttpOnly using a domain prefix.
        let domain = if cookie.http_only {
            format!("#HttpOnly_{}", cookie.host)
        } else {
            cookie.host.clone()
        };

        // Field 1: domain.
        append_field(&mut output, &domain);
        output.push(b'\t');

        // Field 2: whether the domain applies to subdomains.
        append_field(
            &mut output,
            if cookie.host.starts_with('.') {
                "TRUE"
            } else {
                "FALSE"
            },
        );
        output.push(b'\t');

        // Field 3: cookie path.
        append_field(&mut output, &cookie.path);
        output.push(b'\t');

        // Field 4: Secure flag.
        append_field(
            &mut output,
            if cookie.secure { "TRUE" } else { "FALSE" },
        );
        output.push(b'\t');

        // Field 5: Unix expiration timestamp.
        append_field(&mut output, &cookie.expiry.to_string());
        output.push(b'\t');

        // Field 6: cookie name.
        append_field(&mut output, &cookie.name);
        output.push(b'\t');

        // Field 7: cookie value.
        append_field(&mut output, &cookie.value);
        output.push(b'\n');
    }

    // Return the complete cached byte buffer.
    output
}

/// Appends one Netscape field to the output buffer.
///
/// Tabs and newlines are percent-encoded so they cannot create extra
/// columns or records in the tab-separated output format.
fn append_field(
    output: &mut Vec<u8>,
    value: &str,
) {
    // Process the value as UTF-8 bytes.
    for byte in value.bytes() {
        match byte {
            // Encode tabs as `%09`.
            b'\t' => output.extend_from_slice(b"%09"),

            // Encode line feeds as `%0A`.
            b'\n' => output.extend_from_slice(b"%0A"),

            // Encode carriage returns as `%0D`.
            b'\r' => output.extend_from_slice(b"%0D"),

            // Copy all other bytes unchanged.
            _ => output.push(byte),
        }
    }
}
