use context_guardian::passive_capture::{capture_once, CaptureOptions};
use std::env;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

fn main() {
    let options = match Options::from_args() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    loop {
        match capture_once(&options.capture) {
            Ok(path) => println!("passive_capture_report={}", path.display()),
            Err(error) => {
                eprintln!("passive capture failed: {error}");
                if !options.watch {
                    std::process::exit(1);
                }
            }
        }
        if !options.watch {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

struct Options {
    capture: CaptureOptions,
    watch: bool,
}

impl Options {
    fn from_args() -> Result<Self, String> {
        let report_dir = codex_home()?.join("context-guardian/passive-capture-reports");
        let default_tcpdump = if cfg!(target_os = "macos") {
            PathBuf::from("/usr/sbin/tcpdump")
        } else {
            PathBuf::from("/usr/bin/tcpdump")
        };
        let mut options = Self {
            capture: CaptureOptions {
                interface: if cfg!(target_os = "macos") {
                    "lo0"
                } else {
                    "lo"
                }
                .to_string(),
                port: 15721,
                duration: Duration::from_secs(60),
                max_pcap_bytes: 16 * 1024 * 1024,
                max_reports: 100,
                report_dir,
                tcpdump: default_tcpdump,
            },
            watch: false,
        };
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--interface" => options.capture.interface = next(&mut args, &arg)?,
                "--port" => options.capture.port = parse(&next(&mut args, &arg)?, &arg)?,
                "--duration-seconds" => {
                    options.capture.duration =
                        Duration::from_secs(parse(&next(&mut args, &arg)?, &arg)?)
                }
                "--max-pcap-bytes" => {
                    options.capture.max_pcap_bytes = parse(&next(&mut args, &arg)?, &arg)?
                }
                "--max-reports" => {
                    options.capture.max_reports = parse(&next(&mut args, &arg)?, &arg)?
                }
                "--report-dir" => {
                    options.capture.report_dir = PathBuf::from(next(&mut args, &arg)?)
                }
                "--tcpdump" => options.capture.tcpdump = PathBuf::from(next(&mut args, &arg)?),
                "--watch" => options.watch = true,
                "--help" | "-h" => return Err(help()),
                _ => return Err(format!("unknown argument: {arg}\n{}", help())),
            }
        }
        Ok(options)
    }
}

fn next(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn parse<T: std::str::FromStr>(value: &str, flag: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid value for {flag}: {value}"))
}

fn codex_home() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path));
    }
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|path| path.join(".codex"))
        .ok_or_else(|| "set CODEX_HOME, HOME, or USERPROFILE".to_string())
}

fn help() -> String {
    "Usage: context-guardian-passive-capture [--watch] [--interface lo0] [--port 15721] [--duration-seconds 60] [--max-pcap-bytes 16777216] [--max-reports 100] [--report-dir DIR] [--tcpdump PATH]".to_string()
}
