<h3><img width="48" src="resources/app.png" alt="Application icon" /> <code>cookies_from_browser_firefox</code></h3>

A small Rust command-line utility for extracting Firefox cookies from
`cookies.sqlite`, filtering them, sorting them, and writing the result as a
Netscape-format cookie file.

It can also read an existing Netscape cookie file, which makes it useful for
fast post-processing without opening a browser database.

The program writes cookie data to `STDOUT`; diagnostic messages and processing
statistics are written to `STDERR`.

<img src="resources/screenshot_process.png" alt="Program process screenshot" />

Online documentation:

<https://eladkarako.github.io/cookies_from_browser_firefox/>

### What it does

The program:

- Detects whether the input is SQLite or Netscape text format.
- Reads Firefox's `moz_cookies` table.
- Uses multiple blocking worker threads for SQLite extraction.
- Applies include and exclude filters.
- Removes expired cookies when requested.
- Sorts cookies by host, path, and cookie name.
- Writes a Netscape-compatible cookie file to `STDOUT`.
- Preserves `Secure` and `HttpOnly` information.
- Escapes tabs, line feeds, and carriage returns in output fields.

### Input formats

The input can be either:

- A Firefox `cookies.sqlite` database, or another compatible SQLite database
  containing a `moz_cookies` table with these columns:

```text
host
path
isSecure
isHttpOnly
expiry
name
value
```

### A Netscape-format cookie text file.

The input type is detected by reading the first 16 bytes: `SQLite format 3\0`

Files beginning with that SQLite signature are opened as SQLite databases.
All other files are treated as Netscape cookie text files.

Output is always written in Netscape cookie-file format.

### Basic usage (split for better readability)

```txt
cookies_from_browser_firefox
--path "C:\path\to\cookies.sqlite"
> cookies.txt
```

The output can then be passed to another program that accepts Netscape cookie

### Practical usage stories

#### Export only the cookies needed by a downloader

A complete browser cookie database may contain cookies for hundreds of sites.
Use host filters to export only the relevant records:

```
cookies_from_browser_firefox
--path cookies.sqlite
--include-host "youtube.com"
--include-host "google."
> youtube-cookies.txt
```

The include filters are case-insensitive substring matches. The example keeps
cookies whose host contains either `youtube.com` or `google.`

#### Filter an already exported cookie file

The input does not have to be a Firefox database:

```
cookies_from_browser_firefox
--path exported-cookies.txt
--include-host example
--exclude-cookie-name tracking
> filtered-cookies.txt
```

This avoids needing separate `grep`, `findstr`, or similar post-processing commands.

#### Remove expired cookies before exporting

```
cookies_from_browser_firefox
--path cookies.sqlite
--exclude-expired
> active-cookies.txt
```

--exclude-expired without a value uses the current UTC time.

The explicit equivalent is:

```
cookies_from_browser_firefox
--path cookies.sqlite
--exclude-expired expired
> active-cookies.txt
```

#### Keep only secure cookies

```
cookies_from_browser_firefox
--path cookies.sqlite
--include-is-secured
> secure-cookies.txt
```

#### Remove subdomain cookies

```
cookies_from_browser_firefox
--path cookies.sqlite
--exclude-is-subdomain
> host-only-cookies.txt
```

#### Filters - Text filters

Text filters perform case-insensitive substring matching.  
Available filters:  

```
--include-host TEXT
--exclude-host TEXT

--include-path TEXT
--exclude-path TEXT

--include-cookie-name TEXT
--exclude-cookie-name TEXT

--include-cookie-value TEXT
--exclude-cookie-value TEXT
```

#### For example:

```
cookies_from_browser_firefox
--path cookies.sqlite
--include-host example
--include-host mozilla
> included.txt
```

Multiple include filters are combined with OR. A cookie is retained when it
matches at least one include filter.

Multiple exclude filters are also combined with OR. A cookie is removed when
it matches at least one exclude filter.

Include filtering is applied before exclude filtering. Therefore, a cookie
that matches an include filter can still be removed by an exclude filter.

Empty filter values are ignored.

#### Filters - Boolean filters (no value)

```
--include-is-subdomain
--exclude-is-subdomain

--include-is-secured
--exclude-is-secured
```

A cookie is considered a subdomain cookie when its host begins with `.` .

A cookie is considered secured when its `Secure` field is enabled.

Boolean include filters participate in the same `OR` group as the text include
filters. For example:

```
cookies_from_browser_firefox
--path cookies.sqlite
--include-host example
--include-is-secured
> result.txt
```

This includes cookies whose host contains `example` or whose `Secure` flag is.

#### Expiration filtering

The expiration option accepts one or more references:

```
--exclude-expired
--exclude-expired expired
--exclude-expired dateYYYY...
--exclude-expired epochSECONDS
```

When several expiration references are supplied, the oldest reference is used.

Examples:

```
cookies_from_browser_firefox
--path cookies.sqlite
--exclude-expired expired
```

```
cookies_from_browser_firefox
--path cookies.sqlite
--exclude-expired date2026
```

```
cookies_from_browser_firefox
--path cookies.sqlite
--exclude-expired date202609251314
```

```
cookies_from_browser_firefox
--path cookies.sqlite
--exclude-expired epoch1800230732
```

#### Dates are interpreted as UTC. Supported date formats are (Value - Meaning):  

- `date2026`           - `2026-01-01T00:00:00Z`
- `date202609`         - `2026-09-01T00:00:00Z`
- `date20260925`       - `2026-09-25T00:00:00Z`
- `date2026092513`     - `2026-09-25T13:00:00Z`
- `date202609251314`   - `2026-09-25T13:14:00Z`
- `date20260925131400` - `2026-09-25T13:14:00Z`

Epoch values are interpreted as Unix timestamps in seconds. Values shorter than
10 digits are left-padded with zeroes.  

Cookies are excluded when: cookie expiry <= reference time .  

Session cookies have an expiry value of `0`.  
When expiration filtering is enabled,  
they are treated as expired and excluded like ordinary timestamps.  
The program reports the number of expired cookies and the number of session cookies removed.

#### Netscape cookie format

The output starts with a descriptive header and uses these fields:


`domain<TAB>include_subdomains<TAB>path<TAB>secure<TAB>expiration<TAB>name<TAB>value`

Example:

`#HttpOnly_.example.com	TRUE	/	TRUE	1893456000	session	abc123`

This record means:
- `HttpOnly` is enabled because the domain begins with `#HttpOnly_` .
- `include_subdomains` is `TRUE` because the domain begins with `.` .
- `Secure` is enabled because the fourth field is `TRUE` .
- The cookie expires at Unix timestamp `1893456000`.

`HttpOnly` and `Secure` are handled independently.

Tabs, line feeds, and carriage returns inside fields are escaped as:

- tab             - `%09`
- line feed       - `%0A`
- carriage return - `%0D`


#### Sorting

Before output, cookies are sorted by:

1. Host, ignoring a leading `.` (for grouping purposes only, the actual host will still include any `.`) .
2. Path .
3. Cookie name.
4. Original host.
5. Cookie value.
6. Expiration timestamp.
7. Secure.
8. HttpOnly.

Ignoring the leading dot only affects sorting; the original host is preserved in the output.

#### Firefox and yt-dlp

This program can be used instead of `yt-dlp`'s browser-cookie extraction when
working with Firefox and an explicit `cookies.sqlite` path.

```
cookies_from_browser_firefox
--path cookies.sqlite
--include-host youtube
--include-host google
--exclude-expired
> yt-cookies.txt
```

Then use the generated file with:

`yt-dlp --cookies yt-cookies.txt URL`

Passing an already exported Netscape cookie file through the program is also
supported, so the same filtering workflow can be used without reading a
Firefox database.

#### Processing model

SQLite extraction uses Tokio's multi-thread runtime and several blocking
worker tasks:

- The database row count is obtained first.
- The rows are divided between workers.
- Each worker opens the database read-only and reads its assigned range.
- The coordinating task waits for all workers.
- Results are merged, filtered, sorted, and serialized.
- Output is written to `STDOUT`.

The SQLite database is opened with the bundled SQLite library from
`rusqlite`, so an external SQLite installation is not required for normal
builds.

#### Exit codes

```
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
```

#### Build

The project uses Rust 2024 edition.

`cargo build --release`

The optimized release profile enables:

- Link-time optimization
- A single code-generation unit
- Panic aborts
- Stripped symbols
- Reduced binary size
- Disabled incremental compilation

For formatting:

```
cargo fmt -- --check
cargo fmt
```

For documentation:

```
cargo doc --no-deps --document-private-items --release --open
```

<hr/>

#### Complete `--help` entry

```
Usage: cookies_from_browser_firefox [OPTIONS] --path <PATH>

Options:
      --path <PATH>
          Input cookies.sqlite database or Netscape cookie file

      --include-host <TEXT>
          Include cookies whose host contains TEXT

      --exclude-host <TEXT>
          Exclude cookies whose host contains TEXT

      --include-path <TEXT>
          Include cookies whose path contains TEXT

      --exclude-path <TEXT>
          Exclude cookies whose path contains TEXT

      --include-cookie-name <TEXT>
          Include cookies whose name contains TEXT

      --exclude-cookie-name <TEXT>
          Exclude cookies whose name contains TEXT

      --include-cookie-value <TEXT>
          Include cookies whose value contains TEXT

      --exclude-cookie-value <TEXT>
          Exclude cookies whose value contains TEXT

      --include-is-subdomain
          Include cookies whose host begins with '.'

      --exclude-is-subdomain
          Exclude cookies whose host begins with '.'

      --include-is-secured
          Include cookies with the Secure flag enabled

      --exclude-is-secured
          Exclude cookies with the Secure flag enabled

      --exclude-expired [<REFERENCE>]
          Exclude cookies expired at the supplied UTC reference time

  -h, --help
          Print help

  -V, --version
          Print version

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
with OR. Empty text filter values are ignored.
```

<hr/>

#### Build — semi-automatic

`__01_build_releases.cmd` followed by `__02_repack_binary.cmd` builds and
packages the single-file binaries. The packaging script requires `7z.exe` in
the system `PATH`. `version.txt`, `changelog.txt`, `LICENSE` may be added manually.


The first batch file also launches WSL and attempts to run the build commands.
It tries to update everything, but OS-level toolchain dependencies must still
be installed separately.

<hr/>

#### Build — manual

```
rustup update
cargo clean
```

#### For Windows MSVC targets

```
rustup target add x86_64-pc-windows-msvc i686-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
cargo build --release --target i686-pc-windows-msvc
```

Visual Studio Community with the C++ development workload is required for the
Windows MSVC targets.

The project may also contain configuration for Android and Linux targets.
The paths in `.cargo/config.toml` are machine-specific and may need adjustment,
especially for Android NDK and cross-compilation linkers.

#### Linker notes

`.cargo/config.toml` contains linker configuration and additional notes for
cross-compilation.

Depending on the target, the linker may need to be available in `PATH` or
configured with an absolute path. Android builds require an Android NDK
toolchain. Some Linux targets require additional GCC, musl, or cross-compilation packages.

#### `Cargo.toml` release profile

The release profile is configured to reduce binary size and remove unnecessary
runtime information:

```
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
debug = false
strip = true
incremental = false
```

#### Credits

This program was developed with assistance from `GitHub (Microsoft) Copilot (lite)`, `Claude Haiku 4.5`, and `JetBrains' RustRover (Community license)`. 

<hr/>


<hr/>

<details><summary>notes - build related</summary

### build - semi-automatic

`__01_build_releases.cmd` followed by `__02_repack_binary.cmd` will build and zip (needs `7z.exe` in system's `PATH`) the single-file binaries. `version.txt` and `changelog.txt` may be added manually.  

note that the first batch-file also launches wsl and tries to run the build commands.

it always tries to update everything. but it still needs you to install OS toolchain dependencies.

### build - manual

```
rustup update

cargo clean

#note: visual-studio community with C++ development needs to be installed.
rustup target add   x86_64-pc-windows-msvc   i686-pc-windows-msvc
cargo build  --release  --target   x86_64-pc-windows-msvc
cargo build  --release  --target   i686-pc-windows-msvc

#note: that .cargo/config.toml uses specific linker from Android Studio on Windows - paths needs adjustments!
rustup target add   x86_64-linux-android   i686-linux-android   aarch64-linux-android   armv7-linux-androideabi
cargo build  --release  --target   x86_64-linux-android
cargo build  --release  --target   i686-linux-android
cargo build  --release  --target   aarch64-linux-android
cargo build  --release  --target   armv7-linux-androideabi

#note: you'll need apt-get dependencies. and 'aarch64-unknown-linux-musl' uses linker with custom toolchain - paths needs adjustments!.
#sudo apt-get update && sudo apt-get upgrade && sudo apt-get install --yes android-sdk-libsparse-utils apt-fast apt-transport-https aptitude aria2 asciidoc autoconf automake autopoint autotools-dev base-files bash bash-completion binutils binutils-aarch64-linux-gnu binutils-aarch64-linux-gnu-dbg binutils-alpha-linux-gnu binutils-alpha-linux-gnu-dbg binutils-arc-linux-gnu binutils-arc-linux-gnu-dbg binutils-arm-linux-gnueabi binutils-arm-linux-gnueabi-dbg binutils-arm-linux-gnueabihf binutils-arm-linux-gnueabihf-dbg binutils-arm-none-eabi binutils-avr binutils-bpf binutils-common binutils-dev binutils-djgpp binutils-doc binutils-for-build binutils-for-host binutils-h8300-hms binutils-hppa64-linux-gnu binutils-hppa64-linux-gnu-dbg binutils-hppa-linux-gnu binutils-hppa-linux-gnu-dbg binutils-i686-gnu binutils-i686-gnu-dbg binutils-i686-kfreebsd-gnu binutils-i686-kfreebsd-gnu-dbg binutils-i686-linux-gnu binutils-i686-linux-gnu-dbg binutils-ia64-linux-gnu binutils-ia64-linux-gnu-dbg binutils-loongarch64-linux-gnu binutils-loongarch64-linux-gnu-dbg binutils-m68hc1x binutils-m68k-linux-gnu binutils-m68k-linux-gnu-dbg binutils-mingw-w64 binutils-mingw-w64-i686 binutils-mingw-w64-x86-64 binutils-mips64-linux-gnuabi64 binutils-mips64-linux-gnuabi64-dbg binutils-mips64-linux-gnuabin32 binutils-mips64-linux-gnuabin32-dbg binutils-mips64el-linux-gnuabi64 binutils-mips64el-linux-gnuabi64-dbg binutils-mips64el-linux-gnuabin32 binutils-mips64el-linux-gnuabin32-dbg binutils-mips-linux-gnu binutils-mips-linux-gnu-dbg binutils-mipsel-linux-gnu binutils-mipsel-linux-gnu-dbg binutils-mipsisa32r6-linux-gnu binutils-mipsisa32r6-linux-gnu-dbg binutils-mipsisa32r6el-linux-gnu binutils-mipsisa32r6el-linux-gnu-dbg binutils-mipsisa64r6-linux-gnuabi64 binutils-mipsisa64r6-linux-gnuabi64-dbg binutils-mipsisa64r6-linux-gnuabin32 binutils-mipsisa64r6-linux-gnuabin32-dbg binutils-mipsisa64r6el-linux-gnuabi64 binutils-mipsisa64r6el-linux-gnuabi64-dbg binutils-mipsisa64r6el-linux-gnuabin32 binutils-mipsisa64r6el-linux-gnuabin32-dbg binutils-msp430 binutils-multiarch binutils-multiarch-dbg binutils-multiarch-dev binutils-or1k-elf binutils-powerpc64-linux-gnu binutils-powerpc64-linux-gnu-dbg binutils-powerpc64le-linux-gnu binutils-powerpc64le-linux-gnu-dbg binutils-powerpc-linux-gnu binutils-powerpc-linux-gnu-dbg binutils-riscv64-linux-gnu binutils-riscv64-linux-gnu-dbg binutils-riscv64-unknown-elf binutils-s390x-linux-gnu binutils-s390x-linux-gnu-dbg binutils-sh4-linux-gnu binutils-sh4-linux-gnu-dbg binutils-sh-elf binutils-source binutils-sparc64-linux-gnu binutils-sparc64-linux-gnu-dbg binutils-x86-64-gnu binutils-x86-64-gnu-dbg binutils-x86-64-kfreebsd-gnu binutils-x86-64-kfreebsd-gnu-dbg binutils-x86-64-linux-gnu binutils-x86-64-linux-gnu-dbg binutils-x86-64-linux-gnux32 binutils-x86-64-linux-gnux32-dbg binutils-xtensa-lx106 binutils-z80 binwalk bison bsdutils build-essential ca-certificates ccache checkinstall clang clisp-module-zlib cmake cmake-curses-gui cmake-data cmake-doc cmake-extras cmake-fedora cmake-format cmake-qt-gui cmake-vala coreutils curl dash debianutils devscripts dh-autoreconf diffutils docbook2x docbook-xsl docker.io dos2unix doxygen doxygen2man doxygen-awesome-css doxygen-doc doxygen-doxyparse doxygen-gui doxygen-latex dpkg-dev dpkg-dev-el elpa-dpkg-dev-el erlang-p1-zlib erofs-utils erofsfuse expat f2fs-tools findutils flex fuse2fs g++ g++-mingw-w64 g++-mingw-w64-i686 g++-mingw-w64-x86-64 gambas3-gb-compress-bzlib2 gambas3-gb-compress-zlib gcc gcc-aarch64-linux-gnu gcc-arm-linux-gnueabihf gcc-i686-linux-gnu gcc-mingw-w64 gcc-mingw-w64-i686 gcc-mingw-w64-x86-64 gcc-powerpc64-linux-gnu gcc-powerpc64le-linux-gnu gcc-powerpc-linux-gnu gcc-riscv64-linux-gnu gdb-mingw-w64 gedit gettext gfortran-mingw-w64 git glibc-doc glibc-doc-reference glibc-source glibc-tools gnat-mingw-w64 gnome-terminal gobjc-mingw-w64 gobjc++-mingw-w64 golang gperf grep gtk-doc-tools guile-lzlib guile-zlib gyp gzip hostname init intltool libassuan-mingw-w64-dev libattr1 libc6-dev libc6-dev-amd64-cross libc6-dev-amd64-i386-cross libc6-dev-amd64-x32-cross libc6-dev-arm64-cross libc6-dev-armhf-cross libc6-dev-i386 libc6-dev-powerpc-cross libc6-dev-powerpc-ppc64-cross libc6-dev-riscv64-cross libc-ares-dev libc++1 libc++abi1 libcompress-raw-zlib-perl libcppunit-dev libcurl4-openssl-dev libdpkg-dev libdwarf-dev libelf-dev libevent-2.1-7t64 libevent-core-2.1-7t64 libevent-dev libevent-distributor-perl libevent-execflow-perl libevent-extra-2.1-7t64 libevent-openssl-2.1-7t64 libevent-perl libevent-pthreads-2.1-7t64 libevent-rpc-perl libexpat1-dev libexpat-ocaml libexpat-ocaml-dev libffi-dev libfuse3-dev libgcrypt20-dev libgcrypt-mingw-w64-dev libghc-bzlib-dev libghc-bzlib-doc libghc-bzlib-prof libghc-zlib-bindings-dev libghc-zlib-bindings-doc libghc-zlib-bindings-prof libghc-zlib-dev libghc-zlib-doc libghc-zlib-prof libgmp-dev libgnatcoll-zlib3 libgnatcoll-zlib-dev libgnutls28-dev libgpg-error-mingw-w64-dev libguestfs-tools libjansson-dev libjzlib-java libksba-mingw-w64-dev libmpc-dev libmpfr-dev libncurses-dev libnpth-mingw-w64-dev libp11-kit-dev librte-compress-zlib24 libruby3.2 librust-async-compression-dev librust-expat-sys-dev librust-flate2-dev librust-gix-features-dev librust-grcov-dev librust-harfbuzz-sys-dev librust-khronos-egl-dev librust-libsodium-sys-dev librust-libsqlite3-sys-dev librust-libz-sys-dev librust-oxrocksdb-sys-dev librust-pkg-config-dev librust-pq-sys-dev librust-smithay-client-toolkit-dev librust-zip-dev librust-zstd-dev librust-zstd-safe-dev librust-zstd-sys-dev libsgmls-perl libsqlite3-dev libssh2-1-dev libssl-dev libtasn1-6-dev libtool libtool-bin libudev-dev libunistring-dev libxml2-dev libxml-sax-expat-incremental-perl libxml-sax-expatxs-perl libz-mingw-w64 libz-mingw-w64-dev lld llvm-dev login lua-expat lua-expat-dev lua-zlib lua-zlib-dev lzip m4 make mercurial mingw-w64 mingw-w64-common mingw-w64-i686-dev mingw-w64-tools mingw-w64-x86-64-dev musl musl-dev musl-tools nasm nautilus ncurses-base ncurses-bin nettle-dev ninja-build node-browserify-zlib npm openjdk-17-jdk openssh-server p7zip-full p11-kit-doc patch perl pkg-config pkgconf plocate pv python3 python3-colcon-pkg-config python3-docutils python3-jsonschema python3-mako python3-mesonpy python3-pip python3-requests python3-rstr python3-sphinx python-is-python3 r-bioc-zlibbioc ragel re2c ruby-pkg-config screen sed sgml-base sgml-base-doc sgml-data sgml-spell-checker sgmls-doc sgmlspl slang-expat software-properties-common subversion texinfo tree ubuntu-minimal ubuntu-wsl unzip util-linux uuid-dev wget win-iconv-mingw-w64-dev xmlto xsltproc yasm zlib1g-dev
rustup target add   x86_64-unknown-linux-gnu   aarch64-unknown-linux-gnu   x86_64-unknown-linux-musl   aarch64-unknown-linux-musl  powerpc-unknown-linux-gnu  powerpc64-unknown-linux-gnu  powerpc64le-unknown-linux-gnu
cargo build  --release  --target   x86_64-unknown-linux-gnu
cargo build  --release  --target   aarch64-unknown-linux-gnu
cargo build  --release  --target   x86_64-unknown-linux-musl
cargo build  --release  --target   aarch64-unknown-linux-musl
cargo build  --release  --target   powerpc-unknown-linux-gnu
cargo build  --release  --target   powerpc64-unknown-linux-gnu
cargo build  --release  --target   powerpc64le-unknown-linux-gnu
```

### build Docs.

`cargo doc --no-deps --document-private-items --release --open`  

or run `__03_docs.cmd` which also copies a `.nojekyll` (to reduce github's template engine post upload work to zero), root `favicon.ico`, and 'redirect' index.html (from `/resources`) to help the landing page on `doc` folder be shared more easily (it redirects hard-coded to projects' `index.html`).

### code format

`rustfmt.toml` in the project's root 
- to just check use `cargo fmt -- --check` to just check.
- to auto-format use `cargo fmt`
- `rustfmt --print-config default` to see all default.  

or run `__00_format.cmd`


### linker notes

`.cargo/config.toml` - has in-notes, and there are some notes above in the manual build part.  
basically it points to a toolchain - gcc triplet in linux, in `PATH` or custom folder, android ndk folder in (my) windows,  
and adds few useful stuff, for windows x32 binaries, it modifies a flag that forbid accessing more than 2g of RAM,  

### `Cargo.toml` - `[profile.release]`

there are few optimizations for when using `--release`,  
mostly crates are joined inline first, debug symbols is stripped away,  
and trying to reduce the binary size by omitting stuff that were not used.  


</details>

<br/>


Feel free to open an issue, or ask a question.

<a href="https://paypal.me/31adkarak0" target="_blank" rel="noopener noreferrer">
  <img src="https://img.shields.io/badge/Sponsor-Donate-blue?logo=paypal&style=flat" alt="Donate via PayPal">
  <br />
  <img src="https://www.paypalobjects.com/webstatic/mktg/Logo/pp-logo-100px.png" alt="PayPal Donation">
</a>

<br/>
<hr/>
<br/>
